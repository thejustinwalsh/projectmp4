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
    /// Interleaved stereo f32 at `rate`.
    pub pcm: Vec<f32>,
    pub rate: u32,
}

/// Longest window `window` can return as pure silence (frames).
const MAX_SILENT_FRAMES: usize = 1 << 16;
static SILENCE: [f32; MAX_SILENT_FRAMES * 2] = [0.0; MAX_SILENT_FRAMES * 2];

fn default_cache_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/agent-deck"))
}

impl Driver {
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
                    return Ok(Driver { kind: DriverKind::Stems, pcm, rate: PIPELINE_RATE });
                }
            }
        }
        load_mix(src)
    }

    /// `frames` stereo frames starting at `t` seconds, as interleaved samples.
    /// Wholly outside the track (or non-finite/negative `t`) yields silence of
    /// the requested length (capped at 65536 frames). A window straddling the
    /// end is clipped to the track: shorter than requested, never padded.
    pub fn window(&self, t: f64, frames: usize) -> &[f32] {
        let silent = &SILENCE[..frames.min(MAX_SILENT_FRAMES) * 2];
        if !t.is_finite() || t < 0.0 {
            return silent;
        }
        let start = (t * self.rate as f64) as u128 * 2;
        let len = self.pcm.len() as u128;
        if start >= len {
            return silent;
        }
        let start = start as usize;
        let end = (start + frames.saturating_mul(2)).min(self.pcm.len());
        &self.pcm[start..end]
    }
}

fn load_mix(src: &Source) -> Result<Driver> {
    let mut pcm = Vec::new();
    deck_media_codec::visit_stereo(&src.wav, |f| {
        pcm.extend_from_slice(f);
        Ok(())
    })
    .map_err(|e| Error::msg(format!("cannot decode {}: {e}", src.wav.display())))?;
    Ok(Driver { kind: DriverKind::Mix, pcm, rate: PIPELINE_RATE })
}

/// Absolute paths of the drums and bass stems, if a complete, matching bundle exists.
fn find_stems(sha256: &str, cache_dir: &Path) -> Option<[PathBuf; 2]> {
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
    let role = |name: &str| -> Option<PathBuf> {
        let s = m.get("stems")?.as_array()?.iter().find(|s| s.get("role").and_then(Value::as_str) == Some(name))?;
        let ok = s.get("sampleRate")?.as_u64()? == PIPELINE_RATE as u64
            && s.get("channels")?.as_u64()? == 2;
        let p = PathBuf::from(s.get("path")?.as_str()?);
        (ok && p.is_absolute()).then_some(p)
    };
    Some([role("drums")?, role("bass")?])
}

/// Streams drums into one buffer and adds bass into it in place, so peak
/// memory is a single decoded stem plus one packet. Clamps to [-1, 1]. If the
/// stems differ in length the sum covers the shorter.
fn sum_stems(paths: &[PathBuf; 2]) -> Option<Vec<f32>> {
    let mut pcm: Vec<f32> = Vec::new();
    deck_media_codec::visit_stereo(&paths[0], |f| {
        pcm.extend_from_slice(f);
        Ok(())
    })
    .ok()?;
    let mut pos = 0usize;
    deck_media_codec::visit_stereo(&paths[1], |f| {
        let n = f.len().min(pcm.len() - pos);
        for (d, b) in pcm[pos..pos + n].iter_mut().zip(f) {
            *d = (*d + *b).clamp(-1.0, 1.0);
        }
        pos += n;
        Ok(())
    })
    .ok()?;
    if pos < pcm.len() {
        eprintln!("driver: bass stem shorter than drums ({pos} < {} samples); truncating", pcm.len());
        pcm.truncate(pos);
    }
    (!pcm.is_empty()).then_some(pcm)
}
