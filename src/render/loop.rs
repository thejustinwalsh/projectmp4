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
    /// True when every candidate had been skipped and the render carried on
    /// with whatever was on screen.
    pub pool_exhausted: bool,
    /// Longest run of consecutive all-black frames over audible music, before
    /// the overlay is drawn.
    pub max_black_run: u32,
    pub frames: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadEvent {
    pub frame: u64,
    pub preset: PathBuf,
    pub cut: Option<Cut>,
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

/// Walks the preset pool. A load that projectM rejects, or a preset that
/// renders black for over a second of audible music, is skipped for the next
/// in the pool and never tried again this render: a bad preset is bad every
/// time. Running out of candidates is not an error once something is on
/// screen: the render keeps the current preset and says so.
struct Playlist<'a> {
    pool: &'a [PathBuf],
    cursor: usize,
    played: Vec<PathBuf>,
    skipped: Vec<Skipped>,
    exhausted: bool,
}

impl Playlist<'_> {
    fn is_skipped(&self, p: &Path) -> bool {
        self.skipped.iter().any(|s| s.preset == p)
    }

    /// Loads the next usable preset. `Ok(false)`: every candidate has been
    /// skipped, and whatever was on screen stays there.
    fn advance(&mut self, pm: &crate::ffi::Instance<'_>, smooth: bool) -> Result<bool> {
        for _ in 0..self.pool.len() {
            let p = self.pool[self.cursor % self.pool.len()].clone();
            self.cursor += 1;
            if self.is_skipped(&p) {
                continue;
            }
            match pm.load_preset_file(&p, smooth) {
                Ok(()) => {
                    self.played.push(p);
                    return Ok(true);
                }
                Err(e) => {
                    eprintln!("render: skipping {}: {e}", p.display());
                    self.skipped.push(Skipped { preset: p, reason: e.0.into_owned() });
                }
            }
        }
        self.exhausted = true;
        Ok(false)
    }

    /// The current preset drew nothing for a second of music: replace it. If
    /// nothing else loads, it stays (a black picture beats an aborted render).
    fn replace_current(&mut self, pm: &crate::ffi::Instance<'_>, reason: &str) -> Result<()> {
        let Some(cur) = self.played.pop() else { return Ok(()) };
        eprintln!("render: skipping {}: {reason}", cur.display());
        self.skipped.push(Skipped { preset: cur.clone(), reason: reason.into() });
        if !self.advance(pm, false)? {
            // Nothing else is usable: un-skip the one on screen.
            self.skipped.retain(|s| s.preset != cur);
            self.played.push(cur);
        }
        Ok(())
    }
}

/// Peak below this (about -60 dBFS) is silence: black is the right picture.
pub const AUDIBLE_PEAK: f32 = 1e-3;

fn audible(window: &[f32]) -> bool {
    window.iter().any(|s| s.abs() > AUDIBLE_PEAK)
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
    let mut list = Playlist { pool: &plan.pool, cursor: 0, played: Vec::new(), skipped: Vec::new(), exhausted: false };
    let mut loads: Vec<LoadEvent> = Vec::new();
    let switch_at = switch_frames(plan);
    let mut next_switch = 0usize;
    // The opening preset is segment 0; with nothing loadable there is nothing to draw.
    if !list.advance(&pm, false)? {
        return Err(Error::msg(format!("none of the {} candidate presets could be loaded", plan.pool.len())));
    }
    loads.push(LoadEvent { frame: 0, preset: list.played.last().cloned().unwrap_or_default(), cut: None });

    let rate = prep.driver.rate as f64;
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let (mut black_run, mut max_black_run) = (0u32, 0u32);
    let mut last_heard: Option<u64> = None;
    for n in 0..plan.total_frames {
        while next_switch < switch_at.len() && switch_at[next_switch] <= n {
            let cut = plan.switches[next_switch].cut;
            if list.advance(&pm, cut == Cut::Soft)? {
                loads.push(LoadEvent { frame: n, preset: list.played.last().cloned().unwrap_or_default(), cut: Some(cut) });
            }
            next_switch += 1;
            black_run = 0;
        }
        // Audio for this frame; the half-sample keeps the f64 -> index cast from truncating down.
        let first = (n as f64 * rate / fps as f64).round();
        let last = ((n + 1) as f64 * rate / fps as f64).round();
        let window = prep.driver.window((first + 0.5) / rate, (last - first) as usize);
        // Music decays between hits, so a quiet frame is only "silence" once
        // nothing has been audible for half a second.
        if audible(window) {
            last_heard = Some(n);
        }
        let heard = last_heard.is_some_and(|l| n - l < (fps as u64 / 2).max(1));
        pm.pcm_add_float(window, Channels::Stereo);
        pm.set_frame_time(n as f64 / fps as f64);

        let mut px;
        loop {
            pm.render_frame_fbo(gl.fbo());
            px = gl.read_rgba()?;
            if !is_black(&px) {
                black_run = 0;
                break;
            }
            // Black in silence is correct output; only black over music is a dead preset.
            if !heard {
                black_run = 0;
                break;
            }
            black_run += 1;
            max_black_run = max_black_run.max(black_run);
            // A full second of it: a MilkDrop fade is shorter, a dead preset is not.
            if black_run < fps || list.exhausted {
                break;
            }
            list.replace_current(&pm, "rendered black for a second of music")?;
            if list.exhausted {
                break;
            }
            loads.push(LoadEvent { frame: n, preset: list.played.last().cloned().unwrap_or_default(), cut: None });
            black_run = 0;
        }

        flip_rows(&mut px, w, h);
        overlay.draw(&mut px, WIDTH, HEIGHT, bass[n as usize]);
        enc.write_frame(&px)?;
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
        max_black_run,
        frames: plan.total_frames,
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
}
