//! The frame loop: audio window -> projectM -> readback -> flip -> overlay -> ffmpeg.

use super::plan::{prepare, Prepared, RenderArgs, RenderPlan, HEIGHT, WIDTH};
use crate::audio::timeline::{low_band_envelope, Cut, HOP};
use crate::audio::Driver;
use crate::encode::Encoder;
use crate::ffi::{Channels, Error, Result};
use crate::gl_context::GlContext;
use crate::render::Overlay;
use std::path::{Path, PathBuf};

/// A frame whose colour channels are all at or below this is "black".
const BLACK_LEVEL: u8 = 3;
/// Per-pixel-equation mesh handed to projectM.
const MESH: (u32, u32) = (64, 48);

#[derive(Debug, Clone)]
pub struct Skipped {
    pub preset: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct RenderReport {
    pub out: PathBuf,
    pub plan: RenderPlan,
    /// The presets that actually ran, in order. Differs from
    /// `plan.preset_sequence` exactly when a preset was skipped.
    pub played: Vec<PathBuf>,
    pub skipped: Vec<Skipped>,
    /// Every preset that took effect and the frame it first drew on. `cut` is
    /// `None` for the opening preset and for replacements of a dead preset.
    pub loads: Vec<LoadEvent>,
    /// True when every candidate had been set aside as dark and all were
    /// re-admitted. Every skip is still in `skipped`.
    pub pool_exhausted: bool,
    /// Longest run of consecutive all-black frames over audible music, before
    /// the overlay is drawn.
    pub max_black_run: u32,
    pub frames: u64,
    /// Frames actually handed to the encoder; always equals `frames` on success.
    pub frames_written: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadEvent {
    pub frame: u64,
    pub preset: PathBuf,
    pub cut: Option<Cut>,
    /// The `smooth_transition` flag actually passed to projectM.
    pub smooth: bool,
}

pub fn render(args: &RenderArgs) -> Result<PathBuf> {
    render_report(args).map(|r| r.out)
}

pub fn render_report(args: &RenderArgs) -> Result<RenderReport> {
    let prep = prepare(args)?;
    run(args, &prep)
}

/// Per-frame bass level in 0..=1: the sub-200 Hz envelope, normalised so the
/// track's 95th-percentile energy reads 1.0, with a fast attack and a slow
/// release so the watermark pulses rather than flickers.
pub fn bass_per_frame(d: &Driver, fps: u32, frames: u64) -> Vec<f32> {
    let env = low_band_envelope(d);
    let hop_s = HOP as f64 / d.rate as f64;
    let mut sorted = env.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let reference = sorted.get(sorted.len() * 95 / 100).copied().unwrap_or(0.0);
    let mut out = Vec::with_capacity(frames as usize);
    let mut level = 0.0f32;
    for n in 0..frames {
        let lo = ((n as f64 / fps as f64) / hop_s) as usize;
        let hi = (((n + 1) as f64 / fps as f64) / hop_s).ceil() as usize;
        let hi = hi.min(env.len()).max(lo + 1).min(env.len());
        let lo = lo.min(hi.saturating_sub(1));
        let e = if env.is_empty() { 0.0 } else { env[lo..hi].iter().sum::<f64>() / (hi - lo) as f64 };
        let raw = if reference > 0.0 { (e / reference).sqrt().min(1.0) as f32 } else { 0.0 };
        level = raw.max(level * 0.9);
        out.push(level);
    }
    out
}

fn is_black(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).all(|p| p[0] <= BLACK_LEVEL && p[1] <= BLACK_LEVEL && p[2] <= BLACK_LEVEL)
}

/// `glReadPixels` is bottom-up; the video is top-down.
fn flip_rows(px: &mut [u8], w: usize, h: usize) {
    let row = w * 4;
    for y in 0..h / 2 {
        let (top, bottom) = px.split_at_mut((h - 1 - y) * row);
        top[y * row..(y + 1) * row].swap_with_slice(&mut bottom[..row]);
    }
}

/// Walks the preset pool. A load that projectM rejects is skipped for good: a
/// file that does not parse will not parse later. A preset that renders black
/// over a second of music is only *set aside*: it may be the right thing on
/// screen when the music changes, so once every candidate has been set aside
/// they are all re-admitted (`exhausted`). Every skip stays in `skipped`.
struct Playlist<'a> {
    pool: &'a [PathBuf],
    cursor: usize,
    played: Vec<PathBuf>,
    skipped: Vec<Skipped>,
    unloadable: Vec<PathBuf>,
    dark: Vec<PathBuf>,
    exhausted: bool,
    /// The `smooth_transition` argument of the most recent successful load.
    last_smooth: bool,
}

impl Playlist<'_> {
    fn usable(&self, p: &Path) -> bool {
        !self.unloadable.iter().any(|u| u == p) && !self.dark.iter().any(|d| d == p)
    }

    /// Loads the next usable preset. `Ok(false)`: nothing loads at all, and
    /// whatever was on screen stays there.
    fn advance(&mut self, pm: &crate::ffi::Instance<'_>, smooth: bool) -> Result<bool> {
        for pass in 0..2 {
            for _ in 0..self.pool.len() {
                let p = self.pool[self.cursor % self.pool.len()].clone();
                self.cursor += 1;
                if !self.usable(&p) {
                    continue;
                }
                match pm.load_preset_file(&p, smooth) {
                    Ok(()) => {
                        self.played.push(p);
                        self.last_smooth = smooth;
                        return Ok(true);
                    }
                    Err(e) => {
                        eprintln!("render: skipping {}: {e}", p.display());
                        self.skipped.push(Skipped { preset: p.clone(), reason: e.0.into_owned() });
                        self.unloadable.push(p);
                    }
                }
            }
            if pass == 0 && !self.dark.is_empty() {
                // Everything left was set aside for being dark: forgive them all.
                self.dark.clear();
                self.exhausted = true;
            } else {
                break;
            }
        }
        Ok(false)
    }

    /// The current preset drew nothing for a second of music: set it aside and
    /// load another. If nothing loads, it stays on screen. True when a preset
    /// was loaded.
    fn replace_current(&mut self, pm: &crate::ffi::Instance<'_>, reason: &str) -> Result<bool> {
        let Some(cur) = self.played.pop() else { return Ok(false) };
        eprintln!("render: setting aside {}: {reason}", cur.display());
        self.skipped.push(Skipped { preset: cur.clone(), reason: reason.into() });
        self.dark.push(cur.clone());
        if !self.advance(pm, false)? {
            self.dark.retain(|d| *d != cur);
            self.played.push(cur);
            return Ok(false);
        }
        Ok(true)
    }
}

/// Peak below this (about -60 dBFS) is silence: black is the right picture.
pub const AUDIBLE_PEAK: f32 = 1e-3;

fn audible(window: &[f32]) -> bool {
    window.iter().any(|s| s.abs() > AUDIBLE_PEAK)
}

/// Decides when a preset is dead: black for a full second *of music*. Music
/// decays between hits, so audio only counts as silent once nothing has been
/// audible for half a second (`hold`); silence resets the run (a gap longer
/// than the hold means the music stopped, and the next burst starts afresh).
pub struct BlackWatch {
    fps: u32,
    hold: u64,
    run: u32,
    last_heard: Option<u64>,
    /// Longest run seen.
    pub max_run: u32,
}

impl BlackWatch {
    pub fn new(fps: u32) -> BlackWatch {
        BlackWatch { fps, hold: (fps as u64 / 2).max(1), run: 0, last_heard: None, max_run: 0 }
    }

    /// Feed frame `n`'s audio window once per frame, before `observe`.
    pub fn hear(&mut self, n: u64, window: &[f32]) {
        if audible(window) {
            self.last_heard = Some(n);
        }
    }

    fn is_heard(&self, n: u64) -> bool {
        self.last_heard.is_some_and(|l| n - l < self.hold)
    }

    /// One render of frame `n`. True when the run of black music reaches `fps`
    /// frames: the preset should be replaced.
    pub fn observe(&mut self, n: u64, black: bool) -> bool {
        if !black || !self.is_heard(n) {
            self.run = 0;
            return false;
        }
        self.run += 1;
        self.max_run = self.max_run.max(self.run);
        self.run >= self.fps
    }

    pub fn reset(&mut self) {
        self.run = 0;
    }
}

/// First frame at or after each switch time.
pub fn switch_frames(plan: &RenderPlan) -> Vec<u64> {
    plan.switches.iter().map(|s| (s.t * plan.fps as f64 - 1e-9).ceil().max(0.0) as u64).collect()
}

fn run(args: &RenderArgs, prep: &Prepared) -> Result<RenderReport> {
    let plan = &prep.plan;
    let fps = plan.fps;
    let out = args.out_path();

    let overlay = Overlay::new(&plan.title, plan.artist.as_deref(), plan.art.as_deref(), plan.watermark_style, !plan.static_overlay)?;
    let bass = bass_per_frame(&prep.driver, fps, plan.total_frames);

    // `Instance` borrows the context, so the two are separate locals.
    let gl = GlContext::new(WIDTH, HEIGHT)?;
    let pm = gl.create_instance()?;
    pm.set_window_size(WIDTH, HEIGHT);
    pm.set_mesh_size(MESH.0, MESH.1);
    pm.set_fps(fps as i32);
    if let Some(t) = &plan.textures {
        pm.set_texture_search_paths(&[t.as_path()])?;
    }

    let mut enc = Encoder::start(&out, &prep.wav, WIDTH, HEIGHT, fps, plan.fast)?;
    let mut list = Playlist {
        pool: &plan.pool,
        cursor: 0,
        played: Vec::new(),
        skipped: Vec::new(),
        unloadable: Vec::new(),
        dark: Vec::new(),
        exhausted: false,
        last_smooth: false,
    };
    let mut loads: Vec<LoadEvent> = Vec::new();
    let switch_at = switch_frames(plan);
    let mut next_switch = 0usize;
    // The opening preset is segment 0; with nothing loadable there is nothing to draw.
    if !list.advance(&pm, false)? {
        return Err(Error::msg(format!("none of the {} candidate presets could be loaded", plan.pool.len())));
    }
    loads.push(LoadEvent { frame: 0, preset: list.played.last().cloned().unwrap_or_default(), cut: None, smooth: list.last_smooth });

    let rate = prep.driver.rate as f64;
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let mut watch = BlackWatch::new(fps);
    let mut frames_written = 0u64;
    for n in 0..plan.total_frames {
        while next_switch < switch_at.len() && switch_at[next_switch] <= n {
            let cut = plan.switches[next_switch].cut;
            if list.advance(&pm, cut == Cut::Soft)? {
                loads.push(LoadEvent { frame: n, preset: list.played.last().cloned().unwrap_or_default(), cut: Some(cut), smooth: list.last_smooth });
            }
            next_switch += 1;
            watch.reset();
        }
        // Audio for this frame; the half-sample keeps the f64 -> index cast from truncating down.
        let first = (n as f64 * rate / fps as f64).round();
        let last = ((n + 1) as f64 * rate / fps as f64).round();
        let window = prep.driver.window((first + 0.5) / rate, (last - first) as usize);
        watch.hear(n, window);
        pm.pcm_add_float(window, Channels::Stereo);
        pm.set_frame_time(n as f64 / fps as f64);

        let mut px;
        loop {
            pm.render_frame_fbo(gl.fbo());
            px = gl.read_rgba()?;
            if !watch.observe(n, is_black(&px)) {
                break;
            }
            if list.replace_current(&pm, "rendered black for a second of music")? {
                loads.push(LoadEvent { frame: n, preset: list.played.last().cloned().unwrap_or_default(), cut: None, smooth: list.last_smooth });
            }
            watch.reset();
        }

        flip_rows(&mut px, w, h);
        overlay.draw(&mut px, WIDTH, HEIGHT, bass[n as usize]);
        enc.write_frame(&px)?;
        frames_written += 1;
        if args.progress && n % (fps as u64 * 5) == 0 {
            eprintln!("render: frame {n}/{} ({:.0}%)", plan.total_frames, 100.0 * n as f64 / plan.total_frames as f64);
        }
    }
    enc.finish()?;
    Ok(RenderReport {
        out,
        plan: plan.clone(),
        played: list.played,
        skipped: list.skipped,
        loads,
        pool_exhausted: list.exhausted,
        max_black_run: watch.max_run,
        frames: plan.total_frames,
        frames_written,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flip_rows_handles_even_and_odd_heights() {
        for (w, h) in [(3usize, 3usize), (3, 4), (2, 2), (1, 1), (4, 6)] {
            let mut px: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
            let orig = px.clone();
            flip_rows(&mut px, w, h);
            for y in 0..h {
                assert_eq!(px[y * w * 4..(y + 1) * w * 4], orig[(h - 1 - y) * w * 4..(h - y) * w * 4], "{w}x{h} row {y}");
            }
        }
    }

    #[test]
    fn flip_rows_reverses_row_order_and_keeps_pixels_intact() {
        let (w, h) = (3, 3);
        let mut px: Vec<u8> = (0..(w * h * 4) as u8).collect();
        let orig = px.clone();
        flip_rows(&mut px, w, h);
        for y in 0..h {
            assert_eq!(px[y * w * 4..(y + 1) * w * 4], orig[(h - 1 - y) * w * 4..(h - y) * w * 4]);
        }
    }

    // --- BlackWatch: the rule that decides a preset is dead, pinned with exact windows ---

    const FPS: u32 = 60;

    /// One frame's window: constant `amp` (so its peak is `amp`), 800 stereo frames.
    fn win(amp: f32) -> Vec<f32> {
        vec![amp; 1600]
    }

    /// Runs `pattern` (amplitude per frame, all frames black) and returns the
    /// frame index at which the watch first called the preset dead.
    fn dead_at(w: &mut BlackWatch, pattern: impl Iterator<Item = f32>) -> Option<u64> {
        for (n, amp) in pattern.enumerate() {
            let n = n as u64;
            w.hear(n, &win(amp));
            if w.observe(n, true) {
                return Some(n);
            }
        }
        None
    }

    #[test]
    fn dead_exactly_when_a_full_second_of_music_has_been_black() {
        let mut w = BlackWatch::new(FPS);
        assert_eq!(dead_at(&mut w, std::iter::repeat(0.5).take(6000)), Some(59), "the 60th black frame, not the 59th or 61st");
        assert_eq!(w.max_run, 60);
    }

    #[test]
    fn a_visible_frame_resets_the_run() {
        let mut w = BlackWatch::new(FPS);
        for n in 0..59u64 {
            w.hear(n, &win(0.5));
            assert!(!w.observe(n, true));
        }
        w.hear(59, &win(0.5));
        assert!(!w.observe(59, false));
        w.hear(60, &win(0.5));
        assert!(!w.observe(60, true), "the run restarted at 1");
    }

    #[test]
    fn the_audibility_threshold_sits_between_7e4_and_15e4() {
        // 0.0015 (-56 dBFS) is music; 0.0007 (-63 dBFS) is silence. Moving the
        // threshold to 1e-2 or to 1e-4 flips one of these.
        let mut w = BlackWatch::new(FPS);
        assert_eq!(dead_at(&mut w, std::iter::repeat(0.0015).take(6000)), Some(59));
        let mut w = BlackWatch::new(FPS);
        assert_eq!(dead_at(&mut w, std::iter::repeat(0.0007).take(6000)), None);
        // A pad at -45 dBFS (0.0056): the ambient case. Black over it is a dead preset.
        let mut w = BlackWatch::new(FPS);
        assert_eq!(dead_at(&mut w, std::iter::repeat(0.0056).take(6000)), Some(59));
        // Digital silence never counts.
        let mut w = BlackWatch::new(FPS);
        assert_eq!(dead_at(&mut w, std::iter::repeat(0.0).take(6000)), None);
    }

    /// `burst` loud frames then `gap` silent frames, forever.
    fn bursts(burst: usize, gap: usize) -> impl Iterator<Item = f32> {
        (0..).map(move |i| if i % (burst + gap) < burst { 0.5 } else { 0.0 })
    }

    #[test]
    fn a_gap_shorter_than_half_a_second_does_not_break_the_music() {
        // Kicks every 24 frames: the 18-frame gap is bridged, so the run continues.
        let mut w = BlackWatch::new(FPS);
        assert!(dead_at(&mut w, bursts(6, 18).take(600)).is_some());
    }

    #[test]
    fn a_gap_longer_than_the_hold_resets_the_run() {
        // 6 loud + 36 silent: the 36-frame gap outlasts the 30-frame hold, so
        // the music "stopped". Each cycle is at most 36 audible frames: never 60.
        let mut w = BlackWatch::new(FPS);
        assert_eq!(dead_at(&mut w, bursts(6, 36).take(6000)), None);
        assert!(w.max_run < 60);
        // Pausing the run during silence instead of resetting it would reach 60
        // within two cycles (36 + 36): bursts here sit 100 frames apart.
        let mut w = BlackWatch::new(FPS);
        assert_eq!(dead_at(&mut w, bursts(12, 100).take(6000)), None);
        assert!(w.max_run <= 42, "max run {}", w.max_run);
    }
}
