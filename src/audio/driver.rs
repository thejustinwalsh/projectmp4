//! The signal that drives the visuals: summed drums+bass stems when a sibling
//! application (agent-deck) has already separated them, else the full mix.
//!
//! Stem lookup is two hops through agent-deck's read-only data directory:
//!   `stems/requests/<Source.sha256>.json`  -> `{ jobId }`
//!   `stems/bundles/<jobId>/manifest.json`  -> `{ completion, sourceSha256, stems[] }`
//! Any failure along the way is a miss, and a miss means the mix. Nothing here
//! ever writes to that directory.

use crate::audio::normalise::PIPELINE_RATE;
use crate::audio::Source;
use crate::ffi::{Error, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverKind {
    Stems,
    Mix,
}

#[derive(Debug, Clone)]
pub struct Driver {
    pub kind: DriverKind,
    pub rate: u32,
    /// Interleaved stereo f32 at `rate`, followed by `MAX_WINDOW_FRAMES * 2`
    /// zeros. Private: the tail is padding, not signal. Use `samples()`.
    pcm: Vec<f32>,
    /// True length in stereo frames, excluding the padding.
    frames: usize,
}

/// Largest `frames` `window` honours; larger requests are clamped to it. Also
/// the length of the zero tail appended to the buffer at load.
pub const MAX_WINDOW_FRAMES: usize = 1 << 16;

fn default_cache_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/agent-deck"))
}

impl Driver {
    fn new(kind: DriverKind, mut pcm: Vec<f32>) -> Driver {
        let frames = pcm.len() / 2;
        pcm.truncate(frames * 2);
        pcm.resize(frames * 2 + MAX_WINDOW_FRAMES * 2, 0.0);
        Driver { kind, rate: PIPELINE_RATE, pcm, frames }
    }

    /// True length in stereo frames (padding excluded).
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// The decoded signal, interleaved stereo, without the zero padding.
    pub fn samples(&self) -> &[f32] {
        &self.pcm[..self.frames * 2]
    }

    /// `want = None` prefers stems and falls back to the mix; `Some(Mix)`
    /// forces the mix; `Some(Stems)` is the same as `None`.
    pub fn load(src: &Source, want: Option<DriverKind>) -> Result<Driver> {
        match default_cache_dir() {
            Some(dir) => Driver::load_with_cache_dir(src, want, &dir),
            None => load_mix(src),
        }
    }

    /// `cache_dir` is the agent-deck data root (the one containing `stems/`).
    pub fn load_with_cache_dir(
        src: &Source,
        want: Option<DriverKind>,
        cache_dir: &Path,
    ) -> Result<Driver> {
        if want != Some(DriverKind::Mix) {
            if let Some(paths) = find_stems(&src.sha256, cache_dir) {
                // An unreadable stem is a miss, not an error.
                if let Some(pcm) = sum_stems(&paths) {
                    return Ok(Driver::new(DriverKind::Stems, pcm));
                }
            }
        }
        load_mix(src)
    }

    /// Exactly `min(frames, MAX_WINDOW_FRAMES) * 2` interleaved samples
    /// starting at `t` seconds, for every `t` and `frames`. `frames` is clamped
    /// to `MAX_WINDOW_FRAMES`. Any part of the window outside the track
    /// (negative, NaN, infinite or past-the-end `t`, or a straddling window)
    /// is zeros; this never panics.
    pub fn window(&self, t: f64, frames: usize) -> &[f32] {
        let n = frames.min(MAX_WINDOW_FRAMES) * 2;
        let end = self.frames * 2;
        // Compare as f64 before any cast that could saturate.
        let pos = t * self.rate as f64;
        let start = if pos >= 0.0 && pos < self.frames as f64 { pos as usize * 2 } else { end };
        // start <= end, so start + n <= end + MAX_WINDOW_FRAMES * 2 == pcm.len().
        &self.pcm[start..start + n]
    }
}

fn load_mix(src: &Source) -> Result<Driver> {
    let mut pcm = Vec::new();
    deck_media_codec::visit_stereo(&src.wav, |f| {
        pcm.extend_from_slice(f);
        Ok(())
    })
    .map_err(|e| Error::msg(format!("cannot decode {}: {e}", src.wav.display())))?;
    Ok(Driver::new(DriverKind::Mix, pcm))
}

/// Absolute paths of the drums and bass stems, if a complete, matching bundle exists.
fn find_stems(sha256: &str, cache_dir: &Path) -> Option<[(PathBuf, u64); 2]> {
    let stems = cache_dir.join("stems");
    let req: Value =
        serde_json::from_slice(&std::fs::read(stems.join("requests").join(format!("{sha256}.json"))).ok()?)
            .ok()?;
    let job = req.get("jobId")?.as_str()?;
    // jobId becomes a path component: refuse anything that could escape.
    if job.is_empty() || !job.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return None;
    }
    let m: Value = serde_json::from_slice(
        &std::fs::read(stems.join("bundles").join(job).join("manifest.json")).ok()?,
    )
    .ok()?;
    if m.get("completion")?.as_str()? != "complete" || m.get("sourceSha256")?.as_str()? != sha256 {
        return None;
    }
    let role = |name: &str| -> Option<(PathBuf, u64)> {
        let s = m.get("stems")?.as_array()?.iter().find(|s| s.get("role").and_then(Value::as_str) == Some(name))?;
        let ok = s.get("sampleRate")?.as_u64()? == PIPELINE_RATE as u64
            && s.get("channels")?.as_u64()? == 2;
        let p = PathBuf::from(s.get("path")?.as_str()?);
        let frames = s.get("frames")?.as_u64()?;
        (ok && p.is_absolute()).then_some((p, frames))
    };
    Some([role("drums")?, role("bass")?])
}

/// Streams drums into one buffer and adds bass into it in place, so peak
/// memory is a single decoded stem plus one packet. Clamps to [-1, 1]. Each
/// decoded length must equal its manifest `frames` (else a miss: a truncated
/// file must not pass as the whole track). If the stems nonetheless differ in
/// length, the sum covers the shorter, with a warning in either direction.
fn sum_stems(stems: &[(PathBuf, u64); 2]) -> Option<Vec<f32>> {
    let mut pcm: Vec<f32> = Vec::new();
    deck_media_codec::visit_stereo(&stems[0].0, |f| {
        pcm.extend_from_slice(f);
        Ok(())
    })
    .ok()?;
    if (pcm.len() / 2) as u64 != stems[0].1 {
        return None;
    }
    let mut pos = 0usize;
    let mut bass_len = 0usize;
    deck_media_codec::visit_stereo(&stems[1].0, |f| {
        bass_len += f.len();
        let n = f.len().min(pcm.len() - pos);
        for (d, b) in pcm[pos..pos + n].iter_mut().zip(f) {
            *d = (*d + *b).clamp(-1.0, 1.0);
        }
        pos += n;
        Ok(())
    })
    .ok()?;
    if (bass_len / 2) as u64 != stems[1].1 {
        return None;
    }
    if bass_len != pcm.len() {
        eprintln!(
            "driver: stem lengths differ (drums {} vs bass {bass_len} samples); truncating to the shorter",
            pcm.len()
        );
        pcm.truncate(pos);
    }
    (!pcm.is_empty()).then_some(pcm)
}
