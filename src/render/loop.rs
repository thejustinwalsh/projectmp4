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
    /// Longest run of consecutive all-black frames, before the overlay is drawn.
    pub max_black_run: u32,
    pub frames: u64,
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
/// renders black for over a second, is skipped for the next in the pool and
/// never tried again this render: a bad preset is bad every time.
struct Playlist<'a> {
    pool: &'a [PathBuf],
    cursor: usize,
    played: Vec<PathBuf>,
    skipped: Vec<Skipped>,
}

impl Playlist<'_> {
    fn is_skipped(&self, p: &Path) -> bool {
        self.skipped.iter().any(|s| s.preset == p)
    }

    /// Loads the next usable preset; Err when every candidate has been skipped.
    fn advance(&mut self, pm: &crate::ffi::Instance<'_>, smooth: bool) -> Result<()> {
        for _ in 0..self.pool.len() {
            let p = self.pool[self.cursor % self.pool.len()].clone();
            self.cursor += 1;
            if self.is_skipped(&p) {
                continue;
            }
            match pm.load_preset_file(&p, smooth) {
                Ok(()) => {
                    self.played.push(p);
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("render: skipping {}: {e}", p.display());
                    self.skipped.push(Skipped { preset: p, reason: e.0.into_owned() });
                }
            }
        }
        Err(Error::msg(format!("none of the {} candidate presets could be used", self.pool.len())))
    }

    /// The current preset rendered black: move it from played to skipped.
    fn reject_current(&mut self, reason: &str) {
        if let Some(p) = self.played.pop() {
            eprintln!("render: skipping {}: {reason}", p.display());
            self.skipped.push(Skipped { preset: p, reason: reason.into() });
        }
    }
}

fn run(args: &RenderArgs, prep: &Prepared) -> Result<RenderReport> {
    let plan = &prep.plan;
    let fps = plan.fps;
    let out = args.out_path();

    let overlay = Overlay::new(&plan.title, plan.artist.as_deref(), plan.art.as_deref(), args.watermark_style, !args.static_overlay)?;
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

    let mut enc = Encoder::start(&out, &prep.wav, WIDTH, HEIGHT, fps, args.fast)?;
    let mut list = Playlist { pool: &plan.pool, cursor: 0, played: Vec::new(), skipped: Vec::new() };
    // The first frame at or after each switch time.
    let switch_frames: Vec<u64> =
        plan.switches.iter().map(|s| (s.t * fps as f64 - 1e-9).ceil().max(0.0) as u64).collect();
    let mut next_switch = 0usize;
    // The opening preset sits at the plan's cursor 0; the loop below loads it on frame 0.
    list.advance(&pm, false)?;

    let rate = prep.driver.rate as f64;
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let (mut black_run, mut max_black_run) = (0u32, 0u32);
    for n in 0..plan.total_frames {
        while next_switch < switch_frames.len() && switch_frames[next_switch] <= n {
            // The plan's first segment is the opening preset, already loaded.
            let smooth = plan.switches[next_switch].cut == Cut::Soft;
            list.advance(&pm, smooth)?;
            next_switch += 1;
            black_run = 0;
        }
        // Audio for this frame; the half-sample keeps the f64 -> index cast from truncating down.
        let first = (n as f64 * rate / fps as f64).round();
        let last = ((n + 1) as f64 * rate / fps as f64).round();
        pm.pcm_add_float(prep.driver.window((first + 0.5) / rate, (last - first) as usize), Channels::Stereo);
        pm.set_frame_time(n as f64 / fps as f64);

        let mut px;
        loop {
            pm.render_frame_fbo(gl.fbo());
            px = gl.read_rgba()?;
            if !is_black(&px) {
                black_run = 0;
                break;
            }
            black_run += 1;
            max_black_run = max_black_run.max(black_run);
            // More than a second of black: a MilkDrop fade is shorter, a dead preset is not.
            if black_run <= fps {
                break;
            }
            list.reject_current("rendered black for over a second");
            list.advance(&pm, false)?;
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
        max_black_run,
        frames: plan.total_frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
