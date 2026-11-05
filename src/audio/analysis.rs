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

/// What a *working* analyser found. Only an analyser that could not run is an
/// `Err`; anything it returned degrades to something renderable.
#[derive(Debug, Clone)]
pub enum Outcome {
    Beats(Analysis),
    /// Nothing usable to schedule from, with the true reason (no beats found,
    /// or output that could not be read). The caller falls back to a grid and
    /// says why.
    NoBeats(String),
}

pub fn analyse(src: &Source, models: &Path) -> Result<Analysis> {
    match analyse_outcome(src, models)? {
        Outcome::Beats(a) => Ok(a),
        Outcome::NoBeats(why) => Err(Error::msg(format!("beat analysis: {why}"))),
    }
}

/// Like [`analyse`], but a beatless track is `Ok(NoBeats)` while missing or
/// broken models, a failing analyser, or unusable output stay errors.
pub fn analyse_outcome(src: &Source, models: &Path) -> Result<Outcome> {
    let cached = default_cache_dir().and_then(|d| read_cache(&cache_path(&d, src)));
    match cached {
        Some(a) => Ok(Outcome::Beats(a)),
        None => compute(src, models),
    }
}

fn cache_path(dir: &Path, src: &Source) -> PathBuf {
    dir.join("analysis").join(format!("{}.json", src.sha256))
}

/// `cache_dir` is the agent-deck data root; the entry lives at
/// `<cache_dir>/analysis/<sha256>.json`.
pub fn analyse_with_cache_dir(src: &Source, models: &Path, cache_dir: &Path) -> Result<Analysis> {
    if let Some(a) = read_cache(&cache_path(cache_dir, src)) {
        return Ok(a);
    }
    match compute(src, models)? {
        Outcome::Beats(a) => Ok(a),
        Outcome::NoBeats(why) => Err(Error::msg(format!("beat analysis: {why}"))),
    }
}

fn read_cache(path: &Path) -> Option<Analysis> {
    let bytes = std::fs::read(path).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    parse(&v, true)
}

fn compute(src: &Source, models: &Path) -> Result<Outcome> {
    let v = deck_track_analysis::analyze(models, &src.wav).map_err(|e| {
        Error::msg(format!(
            "beat analysis could not run on {}: {e}. The analysis models are expected in {}; \
             check that directory exists and holds the models",
            src.wav.display(),
            models.display()
        ))
    })?;
    match interpret(&v) {
        Interpreted::Usable(a, notes) => {
            for n in notes {
                eprintln!("render: beat analysis: {n}");
            }
            Ok(Outcome::Beats(a))
        }
        Interpreted::Beatless(why) => Ok(Outcome::NoBeats(why)),
    }
}

/// What a fresh analyser result means. Never an error: the analyser ran.
#[derive(Debug)]
enum Interpreted {
    /// Schedulable, with a note for anything that had to be inferred.
    Usable(Analysis, Vec<String>),
    Beatless(String),
}

/// Beats per bar assumed when downbeats must be inferred from beats.
const BEATS_PER_BAR: usize = 4;

/// Reads a fresh `analyze` result leniently. Timestamps at or past the end of
/// the track (a final downbeat a few ms over is normal) are dropped, order and
/// duplicates are repaired, a missing bpm becomes the median beat gap or 0
/// (unknown), and missing downbeats become every 4th beat.
fn interpret(v: &Value) -> Interpreted {
    let Some(duration) = v.get("duration").and_then(Value::as_f64).filter(|d| d.is_finite() && *d > 0.0) else {
        return Interpreted::Beatless("the analyser's result has no usable duration, so it could not be read".into());
    };
    let clean = |key: &str| -> Vec<f64> {
        let mut t: Vec<f64> = v
            .get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        t.retain(|x| x.is_finite() && *x >= 0.0 && *x < duration);
        t.sort_by(|a, b| a.total_cmp(b));
        t.dedup();
        t
    };
    let (beats, mut downbeats) = (clean("beats"), clean("downbeats"));
    if beats.is_empty() && downbeats.is_empty() {
        return Interpreted::Beatless("the analyser found no beats in this track".into());
    }
    let mut notes = Vec::new();
    if downbeats.is_empty() {
        notes.push(format!(
            "{} beats but no downbeats; taking every {BEATS_PER_BAR}th beat as a downbeat",
            beats.len()
        ));
        downbeats = beats.iter().copied().step_by(BEATS_PER_BAR).collect();
    }
    let mut bpm = v.get("bpm").and_then(Value::as_f64).unwrap_or(0.0) as f32;
    if !(bpm.is_finite() && bpm > 0.0) {
        // Median inter-beat interval; 0 (unknown) when there are not two beats.
        let mut gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_by(|a, b| a.total_cmp(b));
        bpm = match gaps.get(gaps.len() / 2) {
            Some(m) if *m > 0.0 => (60.0 / m) as f32,
            _ => 0.0,
        };
    }
    let beats = if beats.is_empty() { downbeats.clone() } else { beats };
    Interpreted::Usable(Analysis { bpm, beats, downbeats, duration, from_cache: false }, notes)
}

fn times(v: &Value, key: &str) -> Option<Vec<f64>> {
    let out: Option<Vec<f64>> = v.get(key)?.as_array()?.iter().map(Value::as_f64).collect();
    out.filter(|t| t.iter().all(|x| x.is_finite()))
}

/// One parser for both the cache entry and a fresh `analyze` result.
fn parse(v: &Value, from_cache: bool) -> Option<Analysis> {
    if !from_cache {
        return match interpret(v) {
            Interpreted::Usable(a, _) => Some(a),
            Interpreted::Beatless(_) => None,
        };
    }
    let beats = times(v, "beats")?;
    let downbeats = times(v, "downbeats")?;
    let duration = v.get("duration")?.as_f64().filter(|d| *d > 0.0)?;
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

    // --- what a fresh analyser result means: it never becomes an error ---

    fn usable(v: &Value) -> (Analysis, Vec<String>) {
        match interpret(v) {
            Interpreted::Usable(a, n) => (a, n),
            Interpreted::Beatless(w) => panic!("expected a usable analysis, got beatless: {w}"),
        }
    }

    #[test]
    fn a_final_downbeat_at_or_past_the_end_is_dropped_not_fatal() {
        let v = json!({"bpm": 120.0, "beats": [0.5, 1.0, 1.5, 10.0, 10.02], "downbeats": [0.5, 2.5, 10.0, 10.03], "duration": 10.0});
        let (a, _) = usable(&v);
        assert_eq!(a.downbeats, vec![0.5, 2.5]);
        assert_eq!(a.beats, vec![0.5, 1.0, 1.5]);
        // A cache entry with the same timestamps is still a miss: there it means corruption.
        assert!(parse(&v, true).is_none());
    }

    #[test]
    fn beats_without_downbeats_give_every_fourth_beat_and_a_bpm_from_the_gaps() {
        let beats: Vec<f64> = (0..10).map(|i| 1.0 + i as f64 * 0.5).collect();
        let v = json!({"bpm": 0.0, "beats": beats, "downbeats": [], "duration": 20.0});
        let (a, notes) = usable(&v);
        assert_eq!(a.downbeats, vec![1.0, 3.0, 5.0]);
        assert!((a.bpm - 120.0).abs() < 1e-3, "bpm {}", a.bpm);
        assert_eq!(notes.len(), 1, "the inference must be reported: {notes:?}");
        assert!(notes[0].contains("downbeat"), "{notes:?}");
    }

    #[test]
    fn downbeats_without_beats_are_enough() {
        let v = json!({"bpm": 0.0, "beats": [], "downbeats": [2.0, 4.0], "duration": 10.0});
        let (a, _) = usable(&v);
        assert_eq!(a.downbeats, vec![2.0, 4.0]);
        assert_eq!(a.bpm, 0.0, "unknown, not invented");
    }

    #[test]
    fn a_single_beat_is_usable_with_an_unknown_bpm() {
        let v = json!({"bpm": 0.0, "beats": [3.0], "downbeats": [], "duration": 10.0});
        let (a, _) = usable(&v);
        assert_eq!((a.bpm, a.downbeats), (0.0, vec![3.0]));
    }

    #[test]
    fn only_a_track_with_nothing_to_schedule_from_is_beatless_and_it_says_why() {
        for v in [
            json!({"bpm": 0.0, "beats": [], "downbeats": [], "duration": 10.0}),
            // Everything the analyser reported lies at or past the end.
            json!({"bpm": 0.0, "beats": [10.0, 11.0], "downbeats": [10.5], "duration": 10.0}),
        ] {
            match interpret(&v) {
                Interpreted::Beatless(w) => assert!(w.contains("no beats"), "{w}"),
                other => panic!("{other:?}"),
            }
        }
        // Output that cannot be read degrades too, and does not claim there were no beats.
        for v in [json!({"beats": [1.0]}), json!({"beats": [1.0], "duration": -3.0}), json!("nonsense")] {
            match interpret(&v) {
                Interpreted::Beatless(w) => assert!(w.contains("could not be read") && !w.contains("no beats"), "{w}"),
                other => panic!("{other:?}"),
            }
        }
    }
}
