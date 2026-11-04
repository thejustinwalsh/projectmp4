//! Beat and downbeat analysis.
//!
//! Reads agent-deck's analysis cache (`analysis/<sha256 of original>.json`)
//! when it holds a usable entry, otherwise runs `deck_track_analysis::analyze`
//! on the normalised WAV (never the original: `deck-media-codec` has no mp3).
//! The cache is read-only; nothing here ever writes to it.

use crate::audio::Source;
use crate::ffi::{Error, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Analysis {
    pub bpm: f32,
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
    pub duration: f64,
    /// True only when the values were parsed from an agent-deck cache file.
    pub from_cache: bool,
}

/// Default location of the analysis models.
pub const DEFAULT_MODELS: &str = "/Users/tjw/Developer/agent-deck/native/track-analysis/models";

/// Root of agent-deck's data directory (the one containing `analysis/`).
fn default_cache_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/agent-deck"))
}

pub fn analyse(src: &Source, models: &Path) -> Result<Analysis> {
    match default_cache_dir() {
        Some(dir) => analyse_with_cache_dir(src, models, &dir),
        None => compute(src, models),
    }
}

/// `cache_dir` is the agent-deck data root; the entry lives at
/// `<cache_dir>/analysis/<sha256>.json`.
pub fn analyse_with_cache_dir(src: &Source, models: &Path, cache_dir: &Path) -> Result<Analysis> {
    let path = cache_dir.join("analysis").join(format!("{}.json", src.sha256));
    if let Some(a) = read_cache(&path) {
        return Ok(a);
    }
    compute(src, models)
}

fn read_cache(path: &Path) -> Option<Analysis> {
    let bytes = std::fs::read(path).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    parse(&v, true)
}

fn compute(src: &Source, models: &Path) -> Result<Analysis> {
    let v = deck_track_analysis::analyze(models, &src.wav)
        .map_err(|e| Error::msg(format!("beat analysis failed on {}: {e}", src.wav.display())))?;
    parse(&v, false).ok_or_else(|| {
        Error::msg("beat analysis returned no usable bpm/beats/downbeats/duration")
    })
}

fn times(v: &Value, key: &str) -> Option<Vec<f64>> {
    let out: Option<Vec<f64>> = v.get(key)?.as_array()?.iter().map(Value::as_f64).collect();
    out.filter(|t| t.iter().all(|x| x.is_finite()))
}

/// One parser for both the cache entry and a fresh `analyze` result.
fn parse(v: &Value, from_cache: bool) -> Option<Analysis> {
    let mut beats = times(v, "beats")?;
    let mut downbeats = times(v, "downbeats")?;
    let duration = v.get("duration")?.as_f64().filter(|d| *d > 0.0)?;
    if !from_cache {
        // A fresh result is put in order rather than rejected; a cache entry
        // is untrusted and must already be ordered, else it is a miss.
        for t in [&mut beats, &mut downbeats] {
            t.sort_by(|a, b| a.total_cmp(b));
            t.dedup();
        }
    }
    if beats.is_empty() || downbeats.is_empty() {
        return None;
    }
    // Consumers (preset switching) rely on a strictly ascending grid that
    // lies inside the track.
    let ascending = |t: &[f64]| t.windows(2).all(|w| w[1] > w[0]);
    if !ascending(&beats) || !ascending(&downbeats) || downbeats.iter().any(|d| *d >= duration) {
        return None;
    }
    let mut bpm = v.get("bpm")?.as_f64()? as f32;
    if !(bpm.is_finite() && bpm > 0.0) {
        // The analyser reports 0.0 when its BPM estimate fails; the beat grid
        // is still good, so take the median inter-beat interval instead.
        let mut gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_by(|a, b| a.total_cmp(b));
        let median = *gaps.get(gaps.len() / 2)?;
        if median <= 0.0 {
            return None;
        }
        bpm = (60.0 / median) as f32;
    }
    Some(Analysis { bpm, beats, downbeats, duration, from_cache })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn zero_bpm_is_taken_from_median_beat_gap() {
        let beats: Vec<f64> = (0..8).map(|i| i as f64 * 0.5).collect();
        let v = json!({"bpm": 0.0, "beats": beats, "downbeats": [0.0, 2.0], "duration": 10.0});
        let a = parse(&v, true).unwrap();
        assert!((a.bpm - 120.0).abs() < 1e-3, "bpm was {}", a.bpm);
    }

    #[test]
    fn zero_bpm_with_under_two_beats_is_rejected() {
        let v = json!({"bpm": 0.0, "beats": [1.0], "downbeats": [1.0], "duration": 10.0});
        assert!(parse(&v, true).is_none());
    }

    #[test]
    fn fresh_result_is_sorted_cache_entry_is_rejected() {
        let v = json!({"bpm": 120.0, "beats": [0.5, 0.0, 1.0], "downbeats": [2.0, 0.0], "duration": 10.0});
        let a = parse(&v, false).unwrap();
        assert_eq!(a.beats, vec![0.0, 0.5, 1.0]);
        assert_eq!(a.downbeats, vec![0.0, 2.0]);
        assert!(parse(&v, true).is_none());
    }
}
