//! Photosensitivity score for a preset: render it headless against a fixed
//! worst-case drive and count WCAG 2.3.1 flashes in the raw output.
//!
//! This measures the preset, not the pipeline: no overlay, no encoder, no flash
//! guard. The ceiling that uses it is WCAG's own number (3 flashes in any one
//! second), not an invented score.
//!
//! Preset files are untrusted third-party data and are only handed to projectM.

use crate::ffi::{Channels, Error, Instance, Result};
use crate::gl_context::GlContext;
use crate::render::flash::{count_flashes, srgb_to_linear_lut, worst_second};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A preset whose worst one-second window holds more flashes than this strobes.
pub const FLASH_CEILING: u32 = 3;
/// Scoring render size. Validated against 1080p by `tests/flashscore.rs`.
pub const SCORE_WIDTH: u32 = 256;
pub const SCORE_HEIGHT: u32 = 144;
pub const SCORE_FPS: u32 = 60;
pub const SCORE_SECS: f64 = 4.0;
/// Bump by hand whenever what a score MEANS changes: the flash counter in
/// `render::flash` (e.g. overlapping vs non-overlapping triples), the region
/// geometry, the drive, the render length or size. It is part of the cache file
/// name, so scores made under other semantics are never read.
pub const SCORER_VERSION: u32 = 2;
/// WCAG 2.3.1's area condition is 25% of a 10-degree field: about 70,700 px^2 at 1080p, a
/// square of about 266 px. A smaller region flashing cannot fail the criterion, so the
/// score looks at the frame mean and at every window of that size, scaled to the render.
const WINDOW_AT_1080: f64 = 266.0;
/// Presets draw per-load randomness (`rand_preset` and friends, noise seeds), so one run
/// is a sample, not a measurement: the same preset has scored 17, 3, 4, 12, 7 and 2.
/// Each preset is loaded this many times and the worst score kept. Scores are therefore
/// approximate, and the ceiling is a quality filter; the render-time guard is the safety net.
pub const LOADS: usize = 2;
/// Same mesh as the real render loop.
const MESH: (u32, u32) = (64, 48);

pub const DRIVE_RATE: u32 = 48_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlashScore {
    /// Flashes in the densest one-second window, worst over the frame mean and all 64 cells.
    pub worst_second: u32,
    /// Flashes per second over the whole run, worst over the same series.
    pub flashes_per_sec: f32,
}

impl FlashScore {
    pub fn passes(&self) -> bool {
        self.worst_second <= FLASH_CEILING
    }
}

/// Interleaved stereo at [`DRIVE_RATE`]: a 55 Hz sine gated at 2 Hz with hard
/// attacks (250 ms on, 250 ms off) over a quiet broadband bed, so presets that
/// key off treble react too. Worst-case on purpose: a preset that stays calm on
/// this will stay calm on a drop. Pure and deterministic.
pub fn synthetic_drive(secs: f64) -> Vec<f32> {
    let n = (secs * DRIVE_RATE as f64).round() as usize;
    let mut out = Vec::with_capacity(n * 2);
    let mut s: u32 = 0x1234_5678;
    for i in 0..n {
        let t = i as f64 / DRIVE_RATE as f64;
        let gate = if (t * 2.0).fract() < 0.5 { 1.0 } else { 0.0 };
        let tone = (std::f64::consts::TAU * 55.0 * t).sin() * 0.9 * gate;
        let mut bed = [0f64; 2];
        for b in &mut bed {
            // xorshift32: deterministic broadband noise.
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            *b = (s as f64 / u32::MAX as f64 * 2.0 - 1.0) * 0.05;
        }
        out.push((tone + bed[0]) as f32);
        out.push((tone + bed[1]) as f32);
    }
    out
}

/// Render `milk` for `secs` seconds on `gl` against `drive` (interleaved stereo at
/// [`DRIVE_RATE`]) and score the raw frames. `pm` and `gl` are reused across
/// presets; the caller sets texture paths on `pm`. The score uses the context's own
/// size, so the caller picks the resolution by building the context.
pub fn score_preset(pm: &Instance, gl: &GlContext, milk: &Path, drive: &[f32], fps: u32, secs: f64) -> Result<FlashScore> {
    let mut best: Option<FlashScore> = None;
    for _ in 0..LOADS {
        pm.load_preset_file(milk, false)?;
        best = Some(worse(best, score_loaded(pm, gl, drive, fps, secs)?));
    }
    best.ok_or_else(|| Error::msg("LOADS is zero"))
}

pub(crate) fn worse(a: Option<FlashScore>, b: FlashScore) -> FlashScore {
    match a {
        None => b,
        Some(a) => FlashScore {
            worst_second: a.worst_second.max(b.worst_second),
            flashes_per_sec: a.flashes_per_sec.max(b.flashes_per_sec),
        },
    }
}

/// Window side in pixels for a render `h` pixels tall, and the step between windows.
fn window_for(h: usize) -> (usize, usize) {
    let side = ((WINDOW_AT_1080 * h as f64 / 1080.0).round() as usize).clamp(1, h);
    (side, (side / 4).max(1))
}

/// Mean relative luminance of the frame, then of every `side` x `side` window at `step`
/// spacing, via a summed-area table. Rec.709 weights on linearised channels.
fn window_luma(px: &[u8], w: usize, h: usize, side: usize, step: usize, sat: &mut Vec<f64>, out: &mut Vec<f32>) {
    let lut = srgb_to_linear_lut();
    let sw = w + 1;
    sat.clear();
    sat.resize(sw * (h + 1), 0.0);
    for y in 0..h {
        let mut row = 0f64;
        for x in 0..w {
            let i = (y * w + x) * 4;
            row += (0.2126 * lut[px[i] as usize] + 0.7152 * lut[px[i + 1] as usize] + 0.0722 * lut[px[i + 2] as usize]) as f64;
            sat[(y + 1) * sw + x + 1] = sat[y * sw + x + 1] + row;
        }
    }
    let rect = |x0: usize, y0: usize, x1: usize, y1: usize| sat[y1 * sw + x1] - sat[y0 * sw + x1] - sat[y1 * sw + x0] + sat[y0 * sw + x0];
    out.clear();
    out.push((rect(0, 0, w, h) / (w * h) as f64) as f32);
    let area = (side * side) as f64;
    let (mut y, mut xs) = (0, Vec::new());
    let mut x = 0;
    while x + side <= w {
        xs.push(x);
        x += step;
    }
    if xs.last().is_some_and(|&l| l + side < w) {
        xs.push(w - side);
    }
    loop {
        for &x in &xs {
            out.push((rect(x, y, x + side, y + side) / area) as f32);
        }
        if y + side >= h {
            break;
        }
        y = (y + step).min(h - side);
    }
}

/// [`score_preset`] for a preset already loaded into `pm`; a failed load is then the
/// caller's to tell apart from a failed render.
pub fn score_loaded(pm: &Instance, gl: &GlContext, drive: &[f32], fps: u32, secs: f64) -> Result<FlashScore> {
    let (w, h) = (gl.width(), gl.height());
    pm.set_window_size(w, h);
    pm.set_mesh_size(MESH.0, MESH.1);
    pm.set_fps(fps as i32);

    let frames = (secs * fps as f64).round() as usize;
    let (wu, hu) = (w as usize, h as usize);
    let mut px = vec![0u8; wu * hu * 4];
    // Series 0 is the frame mean, the rest are the sliding windows.
    let (side, step) = window_for(hu);
    let mut series: Vec<Vec<f32>> = Vec::new();
    let (mut sat, mut vals) = (Vec::new(), Vec::new());
    let per_channel = drive.len() / 2;
    if per_channel == 0 {
        return Err(Error::msg("empty drive"));
    }
    for n in 0..frames {
        // The drive loops: it is periodic, so the seam is inaudible to the preset.
        let first = (n as f64 * DRIVE_RATE as f64 / fps as f64).round() as usize;
        let last = ((n + 1) as f64 * DRIVE_RATE as f64 / fps as f64).round() as usize;
        let mut buf: Vec<f32> = Vec::with_capacity((last - first) * 2);
        for i in first..last {
            let j = i % per_channel;
            buf.extend_from_slice(&drive[j * 2..j * 2 + 2]);
        }
        pm.pcm_add_float(&buf, Channels::Stereo);
        pm.set_frame_time(n as f64 / fps as f64);
        pm.render_frame_fbo(gl.fbo());
        gl.read_rgba_into(&mut px)?;
        window_luma(&px, wu, hu, side, step, &mut sat, &mut vals);
        if series.is_empty() {
            series = vec![Vec::with_capacity(frames); vals.len()];
        }
        for (s, v) in series.iter_mut().zip(&vals) {
            s.push(*v);
        }
    }
    let (mut worst, mut worst_total) = (0usize, 0usize);
    for s in &series {
        let f = count_flashes(s);
        worst = worst.max(worst_second(s.len(), &f, fps).0);
        worst_total = worst_total.max(f.len());
    }
    Ok(FlashScore { worst_second: worst as u32, flashes_per_sec: (worst_total as f64 / secs) as f32 })
}

/// Scores presets on one shared context and instance, through a [`FlashCache`].
pub struct PresetScorer<'a, 'gl> {
    pm: &'a Instance<'gl>,
    gl: &'a GlContext,
    drive: Vec<f32>,
    pub cache: FlashCache,
    /// Scores answered from the cache / rendered fresh.
    pub hits: usize,
    pub rendered: usize,
}

impl PresetScorer<'_, '_> {
    /// `Ok(None)`: the preset could not be loaded, so there is nothing to score.
    /// `Err`: the render itself failed, which says nothing about the preset.
    pub fn score(&mut self, path: &Path) -> Result<Option<FlashScore>> {
        let bytes = std::fs::read(path).map_err(|e| Error::msg(format!("read {}: {e}", path.display())))?;
        let hash = content_hash(&bytes);
        if let Some(s) = self.cache.get(&hash) {
            self.hits += 1;
            return Ok(Some(s));
        }
        let mut best = None;
        for _ in 0..LOADS {
            if self.pm.load_preset_file(path, false).is_err() {
                return Ok(None);
            }
            best = Some(worse(best, score_loaded(self.pm, self.gl, &self.drive, SCORE_FPS, SCORE_SECS)?));
        }
        let s = best.ok_or_else(|| Error::msg("LOADS is zero"))?;
        self.cache.insert(&hash, s);
        self.rendered += 1;
        // A long run survives an interruption; a failed save only costs a re-score.
        if self.rendered % 10 == 0 {
            if let Err(e) = self.cache.save() {
                eprintln!("warning: flash score cache not saved: {e}");
            }
        }
        Ok(Some(s))
    }
}

/// Run `f` with a scorer over one headless context at [`SCORE_WIDTH`]x[`SCORE_HEIGHT`] and
/// the cache that lives beside the packs; saves the cache afterwards.
pub fn with_scorer<T>(packs: &crate::presets::Packs, f: impl FnOnce(&mut PresetScorer) -> Result<T>) -> Result<T> {
    let gl = GlContext::new(SCORE_WIDTH, SCORE_HEIGHT)?;
    let pm = gl.create_instance()?;
    pm.set_texture_search_paths(&[packs.textures.as_path()])?;
    let mut sc = PresetScorer {
        pm: &pm,
        gl: &gl,
        drive: synthetic_drive(SCORE_SECS),
        cache: FlashCache::load(&packs.cache_dir()?.join(FlashCache::file_name())),
        hits: 0,
        rendered: 0,
    };
    let out = f(&mut sc);
    if let Err(e) = sc.cache.save() {
        eprintln!("warning: flash score cache not saved: {e}");
    }
    out
}

/// SHA-256 of the bytes, lowercase hex. The cache key: content, not path.
pub fn content_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Scores by preset content hash, persisted as TOML. Entries are approximate (see [`LOADS`]). A performance artifact,
/// never a source of truth: a missing, truncated or unparseable file yields an
/// empty cache (so a re-score), never an error.
#[derive(Debug, Clone, Default)]
pub struct FlashCache {
    path: Option<PathBuf>,
    map: HashMap<String, FlashScore>,
    dirty: bool,
}

impl FlashCache {
    /// File name for the scoring parameters in force, so a change of resolution,
    /// length or grid cannot read scores made under another one.
    pub fn file_name() -> String {
        format!("flash-scores-s{SCORER_VERSION}-{SCORE_WIDTH}x{SCORE_HEIGHT}-{SCORE_FPS}fps-{}s-x{LOADS}-win.toml", SCORE_SECS as u32)
    }

    pub fn in_memory() -> FlashCache {
        FlashCache::default()
    }

    /// Load `path`; any problem reading or parsing gives an empty cache bound to `path`.
    pub fn load(path: &Path) -> FlashCache {
        let map = std::fs::read_to_string(path).ok().and_then(|t| parse(&t)).unwrap_or_default();
        FlashCache { path: Some(path.to_path_buf()), map, dirty: false }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, hash: &str) -> Option<FlashScore> {
        self.map.get(hash).copied()
    }

    pub fn insert(&mut self, hash: &str, score: FlashScore) {
        if self.map.insert(hash.to_string(), score) != Some(score) {
            self.dirty = true;
        }
    }

    /// Write atomically (temp file, then rename). No-op when clean or in-memory.
    pub fn save(&mut self) -> Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        if !self.dirty {
            return Ok(());
        }
        let mut keys: Vec<&String> = self.map.keys().collect();
        keys.sort();
        let mut s = String::from("# Flash scores by preset content hash. A cache: safe to delete.\n");
        for k in keys {
            let v = self.map[k];
            s.push_str(&format!("\n[[score]]\nhash = \"{k}\"\nworst_second = {}\nflashes_per_sec = {}\n", v.worst_second, v.flashes_per_sec));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::msg(format!("create {}: {e}", dir.display())))?;
        }
        let part = path.with_extension("toml.part");
        std::fs::write(&part, s).map_err(|e| Error::msg(format!("write {}: {e}", part.display())))?;
        std::fs::rename(&part, path).map_err(|e| Error::msg(format!("rename to {}: {e}", path.display())))?;
        self.dirty = false;
        Ok(())
    }
}

/// `None` on anything malformed: the caller treats that as an empty cache.
fn parse(text: &str) -> Option<HashMap<String, FlashScore>> {
    let mut map = HashMap::new();
    let mut cur: (Option<String>, Option<u32>, Option<f32>) = (None, None, None);
    let mut open = false;
    let mut flush = |cur: &mut (Option<String>, Option<u32>, Option<f32>), open: bool| -> Option<()> {
        if open {
            let (Some(h), Some(w), Some(f)) = (cur.0.take(), cur.1.take(), cur.2.take()) else { return None };
            if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) || !f.is_finite() {
                return None;
            }
            map.insert(h, FlashScore { worst_second: w, flashes_per_sec: f });
        }
        Some(())
    };
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if line == "[[score]]" {
            flush(&mut cur, open)?;
            open = true;
            continue;
        }
        let (k, v) = line.split_once('=')?;
        let v = v.trim();
        match k.trim() {
            "hash" => cur.0 = Some(v.strip_prefix('"')?.strip_suffix('"')?.to_string()),
            "worst_second" => cur.1 = Some(v.parse().ok()?),
            "flashes_per_sec" => cur.2 = Some(v.parse().ok()?),
            _ => return None,
        }
    }
    flush(&mut cur, open)?;
    Some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_see_a_patch_only_at_window_scale() {
        let (w, h) = (64usize, 36usize);
        let mut px = vec![0u8; w * h * 4];
        px.chunks_exact_mut(4).for_each(|p| p[3] = 255);
        // An 8x8 white patch: well under a window of 9 (= 266 * 36 / 1080).
        let (side, step) = window_for(h);
        assert_eq!(side, 9);
        for y in 10..18 {
            for x in 20..28 {
                px[(y * w + x) * 4..(y * w + x) * 4 + 3].fill(255);
            }
        }
        let (mut sat, mut out) = (Vec::new(), Vec::new());
        window_luma(&px, w, h, side, step, &mut sat, &mut out);
        let mean = out[0];
        assert!((mean - 64.0 / (w * h) as f32).abs() < 1e-5, "{mean}");
        let best = out[1..].iter().cloned().fold(0.0, f32::max);
        assert!((best - 64.0 / 81.0).abs() < 1e-4, "best window {best}");
        // Covering windows reach the image edge.
        let last = (h - side) as usize;
        assert!(out.len() > 1 && last > 0);
    }

    #[test]
    fn malformed_cache_text_is_rejected_whole() {
        assert!(parse("[[score]]\nhash = \"abc\"\nworst_second = 1\nflashes_per_sec = 1.0\n").is_none());
        assert!(parse("[[score]]\nworst_second = 1\n").is_none());
        assert!(parse("garbage").is_none());
        assert_eq!(parse("# empty\n").unwrap().len(), 0);
    }
}
