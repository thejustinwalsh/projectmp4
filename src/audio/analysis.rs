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
pub const DEFAULT_MODELS: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/crates/track-analysis/models");

/// Root of agent-deck's data directory (the one containing `analysis/`).
///
/// Named for what it is, not `default_cache_dir`: that name belongs to the
/// *writable* projectmp4 preset cache in `presets::packs`, and this directory is
/// read-only to this program. Nothing in this module may write under it.
fn agent_deck_dir() -> Option<PathBuf> {
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
    match analyse_outcome(src, models, None)? {
        Outcome::Beats(a) => Ok(a),
        Outcome::NoBeats(why) => Err(Error::msg(format!("beat analysis: {why}"))),
    }
}

/// Like [`analyse`], but a beatless track is `Ok(NoBeats)` while missing or
/// broken models, a failing analyser, or unusable output stay errors.
///
/// `track_secs` is the length of the audio the analyser was run on (the
/// normalised WAV, i.e. the *decoded* length: a container's own duration can be
/// an estimate that is off by seconds on VBR MP3s). An analyser reporting a
/// different length has analysed the wrong span and is not trusted.
pub fn analyse_outcome(src: &Source, models: &Path, track_secs: Option<f64>) -> Result<Outcome> {
    match agent_deck_dir() {
        Some(d) => analyse_outcome_with_cache_dir(src, models, &d, track_secs),
        None => compute(src, models, track_secs),
    }
}

fn cache_path(dir: &Path, src: &Source) -> PathBuf {
    dir.join("analysis").join(format!("{}.json", src.sha256))
}

/// [`analyse_outcome`] against an explicit agent-deck data root: the entry lives
/// at `<cache_dir>/analysis/<sha256>.json`. This is the one body both production
/// and tests run, so a test through it exercises the same cache check and the
/// same duration guard as a real render. A cached entry whose duration differs
/// from `track_secs` by more than [`DURATION_TOLERANCE`] is a miss.
pub fn analyse_outcome_with_cache_dir(
    src: &Source,
    models: &Path,
    cache_dir: &Path,
    track_secs: Option<f64>,
) -> Result<Outcome> {
    match read_cache(&cache_path(cache_dir, src), track_secs) {
        Some(o) => Ok(o),
        None => compute(src, models, track_secs),
    }
}

fn read_cache(path: &Path, track_secs: Option<f64>) -> Option<Outcome> {
    let bytes = std::fs::read(path).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    let (a, note) = parse_cache(&v)?;
    // The key says nothing about length, and a duration that is too short drops
    // every downbeat past it: the rest of the track would sit on one preset. A
    // fresh analysis is the right answer to a disagreement, not a truncation.
    if let Some(r) = track_secs.filter(|r| (a.duration - r).abs() > DURATION_TOLERANCE) {
        eprintln!(
            "render: beat analysis: cached analysis says {:.1} s but the track is {r:.1} s; ignoring it and analysing afresh",
            a.duration
        );
        return None;
    }
    if let Some(n) = note {
        eprintln!("render: beat analysis: {n}");
    }
    if a.downbeats.is_empty() {
        return Some(Outcome::NoBeats("the cached analysis found no beats in this track".into()));
    }
    Some(Outcome::Beats(a))
}

fn compute(src: &Source, models: &Path, track_secs: Option<f64>) -> Result<Outcome> {
    let v = deck_track_analysis::analyze(models, &src.wav).map_err(|e| {
        Error::msg(format!(
            "beat analysis could not run on {}: {e}. The analysis models are expected in {}; \
             check that directory exists and holds the models",
            src.wav.display(),
            models.display()
        ))
    })?;
    match interpret(&v, track_secs) {
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

/// Seconds by which the analyser's duration may differ from the track's before
/// its schedule is not trusted: the beats would be cut off or stretched.
pub(crate) const DURATION_TOLERANCE: f64 = 1.0;

/// One timestamp array of an analyser result, cleaned and accounted for.
struct Cleaned {
    t: Vec<f64>,
    present: bool,
    /// Entries that were not finite numbers.
    garbage: usize,
    notes: Vec<String>,
    /// Timestamps dropped for lying outside `0..duration`.
    out_of_range: usize,
}

fn clean(v: &Value, key: &str, duration: f64) -> Cleaned {
    let arr = v.get(key).and_then(Value::as_array);
    let mut c = Cleaned { t: Vec::new(), present: arr.is_some(), garbage: 0, notes: Vec::new(), out_of_range: 0 };
    for x in arr.into_iter().flatten() {
        match x.as_f64().filter(|f| f.is_finite()) {
            Some(f) => c.t.push(f),
            None => c.garbage += 1,
        }
    }
    if c.garbage > 0 {
        c.notes.push(format!("{key}: {} entries were not numbers and were ignored", c.garbage));
    }
    let before = c.t.len();
    c.t.retain(|x| *x >= 0.0 && *x < duration);
    c.out_of_range = before - c.t.len();
    if c.out_of_range > 0 {
        c.notes.push(format!("{key}: {} timestamps outside the {duration:.1} s track were dropped", c.out_of_range));
    }
    if c.t.windows(2).any(|w| w[1] < w[0]) {
        c.notes.push(format!("{key}: out of order, sorted"));
    }
    c.t.sort_by(|a, b| a.total_cmp(b));
    let n = c.t.len();
    c.t.dedup();
    if c.t.len() < n {
        c.notes.push(format!("{key}: {} duplicate timestamps removed", n - c.t.len()));
    }
    c
}

/// Reads a fresh `analyze` result leniently, and says what it had to repair.
/// Timestamps at or past the end of the track (a final downbeat a few ms over
/// is normal) are dropped, order and duplicates repaired, a missing bpm becomes
/// the median beat gap or 0 (unknown), and missing downbeats become every 4th
/// beat. `reference` is the track's own duration: an analyser that reports a
/// different one (over `DURATION_TOLERANCE`) has analysed the wrong span, and
/// its schedule is not used.
fn interpret(v: &Value, reference: Option<f64>) -> Interpreted {
    let Some(duration) = v.get("duration").and_then(Value::as_f64).filter(|d| d.is_finite() && *d > 0.0) else {
        return Interpreted::Beatless("the analyser's result has no usable duration, so it could not be read".into());
    };
    if let Some(r) = reference.filter(|r| (duration - r).abs() > DURATION_TOLERANCE) {
        return Interpreted::Beatless(format!(
            "the analyser reported a duration of {duration:.1} s for a {r:.1} s track, so its beats would cover the wrong span; not used"
        ));
    }
    let (b, d) = (clean(v, "beats", duration), clean(v, "downbeats", duration));
    let mut notes: Vec<String> = b.notes.iter().chain(&d.notes).cloned().collect();
    let (beats, mut downbeats) = (b.t, d.t);
    if beats.is_empty() && downbeats.is_empty() {
        let garbage = b.garbage + d.garbage;
        return Interpreted::Beatless(if garbage > 0 {
            format!("the analyser's beats could not be read: {garbage} entries were not numbers")
        } else if !b.present && !d.present {
            "the analyser's result has neither beats nor downbeats, so it could not be read".into()
        } else if b.out_of_range + d.out_of_range > 0 {
            format!("every beat the analyser reported lies outside the {duration:.1} s track")
        } else {
            "the analyser found no beats in this track".into()
        });
    }
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

/// A cache entry: the same analyser wrote it, so a final timestamp a few ms past
/// the end is dropped like in a fresh result (rejecting it would force a slow
/// re-analysis on every run).
///
/// Strictness sits on the two fields that reach the screen. `downbeats` is the
/// switch schedule and `duration` bounds it, so a non-numeric, non-finite,
/// negative, out-of-order or repeated downbeat, or a missing or non-positive
/// duration, is a miss, as is an entry whose downbeats all lie past its own
/// end. `beats` and `bpm` drive no pixel (`bpm` only labels the plan), so they
/// are read leniently: cleaned, never a reason to throw the entry away.
///
/// An entry with both arrays genuinely empty is a real answer, "this track has
/// no beats", and is returned as an `Analysis` with no downbeats; refusing it
/// would send every ambient render back through the slow analyser.
fn parse_cache(v: &Value) -> Option<(Analysis, Option<String>)> {
    let duration = v.get("duration")?.as_f64().filter(|d| d.is_finite() && *d > 0.0)?;
    let mut downbeats = times(v, "downbeats")?;
    let ascending = downbeats.windows(2).all(|w| w[1] > w[0]);
    if !ascending || downbeats.iter().any(|t| *t < 0.0) {
        return None;
    }
    let b = clean(v, "beats", duration);
    if !b.present {
        return None;
    }
    let beats = b.t;
    let stored = downbeats.len();
    downbeats.retain(|t| *t < duration);
    let dropped = stored - downbeats.len();
    let mut note = (dropped > 0).then(|| format!("cached analysis: {dropped} timestamps at or past the end of the track dropped"));
    if downbeats.is_empty() {
        if stored > 0 {
            return None; // every downbeat past its own duration: stale
        }
        if beats.is_empty() {
            let stored_beats = v.get("beats").and_then(Value::as_array).map_or(0, Vec::len);
            if stored_beats > 0 {
                return None; // beats were stored but none is usable: corrupt, not beatless
            }
            return Some((Analysis { bpm: 0.0, beats, downbeats, duration, from_cache: true }, None));
        }
        downbeats = beats.iter().copied().step_by(BEATS_PER_BAR).collect();
        note = Some(format!("cached analysis: {} beats but no downbeats; taking every {BEATS_PER_BAR}th beat as a downbeat", beats.len()));
    }
    let mut bpm = v.get("bpm").and_then(Value::as_f64).unwrap_or(0.0) as f32;
    if !(bpm.is_finite() && bpm > 0.0) {
        // The analyser reports 0.0 when its BPM estimate fails; the beat grid
        // is still good, so take the median inter-beat interval instead. With
        // under two beats it stays 0 (unknown): it labels the plan, nothing more.
        let mut gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_by(|a, b| a.total_cmp(b));
        bpm = match gaps.get(gaps.len() / 2) {
            Some(m) if *m > 0.0 => (60.0 / m) as f32,
            _ => 0.0,
        };
    }
    let beats = if beats.is_empty() { downbeats.clone() } else { beats };
    Some((Analysis { bpm, beats, downbeats, duration, from_cache: true }, note))
}

#[cfg(test)]
/// Test seam: `from_cache` picks the cache or the fresh reading.
fn parse(v: &Value, from_cache: bool) -> Option<Analysis> {
    if from_cache {
        return parse_cache(v).map(|(a, _)| a);
    }
    match interpret(v, None) {
        Interpreted::Usable(a, _) => Some(a),
        Interpreted::Beatless(_) => None,
    }
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
    fn zero_bpm_with_under_two_beats_is_kept_with_an_unknown_bpm() {
        // bpm labels the plan JSON and drives no pixel: not a reason to discard the schedule.
        let v = json!({"bpm": 0.0, "beats": [1.0], "downbeats": [1.0], "duration": 10.0});
        let a = parse(&v, true).expect("a usable schedule must not be thrown away over bpm");
        assert_eq!((a.bpm, a.downbeats), (0.0, vec![1.0]));
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
        match interpret(v, None) {
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
        // The same applies to a cache entry written by that analyser: dropped, not a miss.
        let c = parse(&v, true).expect("a cached final downbeat past the end must not force a re-analysis");
        assert_eq!(c.downbeats, vec![0.5, 2.5]);
        // What marks a cache entry as bad is damage to the fields that reach the screen:
        // the downbeats (repeats, disorder, negatives, garbage, all past the end) and the duration.
        for bad in [
            json!({"bpm": 120.0, "beats": [0.5, 1.0], "downbeats": [0.5, 0.5], "duration": 10.0}),
            json!({"bpm": 120.0, "beats": [0.5, 1.0], "downbeats": [1.0, 0.5], "duration": 10.0}),
            json!({"bpm": 120.0, "beats": [0.5, 1.0], "downbeats": [-0.5, 1.0], "duration": 10.0}),
            json!({"bpm": 120.0, "beats": [10.5], "downbeats": [10.5], "duration": 10.0}),
            json!({"bpm": 120.0, "beats": [0.5, 1.0], "downbeats": ["x", 1.0], "duration": 10.0}),
            json!({"bpm": 120.0, "beats": [0.5, 1.0], "downbeats": [1.0]}),
            json!({"bpm": 120.0, "beats": [0.5, 1.0], "downbeats": [1.0], "duration": 0.0}),
            json!({"bpm": 120.0, "downbeats": [1.0], "duration": 10.0}),
            // Beats stored, none usable, no downbeats: corrupt, not "beatless".
            json!({"bpm": 120.0, "beats": ["x", -1.0], "downbeats": [], "duration": 10.0}),
        ] {
            assert!(parse(&bad, true).is_none(), "{bad}");
        }
    }

    #[test]
    fn a_damaged_beats_array_does_not_cost_the_cache_entry() {
        // `beats` reaches no pixel; discarding the entry would force a full neural
        // re-analysis on every future run of the track, for data that is then unused.
        for beats in [json!([0.5, 0.5, 1.0]), json!([1.0, 0.5]), json!([-0.5, 1.0]), json!(["x", 1.0]), json!([12.0, 1.0])] {
            let v = json!({"bpm": 120.0, "beats": beats, "downbeats": [1.0, 3.0], "duration": 10.0});
            let a = parse(&v, true).unwrap_or_else(|| panic!("entry discarded over beats {beats}"));
            assert_eq!(a.downbeats, vec![1.0, 3.0], "{beats}");
            assert!(a.beats.windows(2).all(|w| w[1] > w[0]), "{beats} -> {:?}", a.beats);
        }
    }

    #[test]
    fn a_beatless_analysis_can_be_cached() {
        // The cache must be able to say "this track has no beats", or an ambient
        // track re-runs the analyser on every render, forever.
        let v = json!({"bpm": 0.0, "beats": [], "downbeats": [], "duration": 90.0});
        let (a, note) = parse_cache(&v).expect("a legitimately beatless entry must be a hit");
        assert!(a.downbeats.is_empty() && a.beats.is_empty() && a.from_cache && a.duration == 90.0);
        assert!(note.is_none());
        // Beats without downbeats in the cache follow the fresh-result rule.
        let v = json!({"bpm": 0.0, "beats": [1.0, 1.5, 2.0, 2.5, 3.0], "downbeats": [], "duration": 10.0});
        let (a, note) = parse_cache(&v).unwrap();
        assert_eq!(a.downbeats, vec![1.0, 3.0]);
        assert!(note.unwrap().contains("downbeat"));
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

    fn why(v: &Value, reference: Option<f64>) -> String {
        match interpret(v, reference) {
            Interpreted::Beatless(w) => w,
            other => panic!("expected a fallback, got {other:?}"),
        }
    }

    #[test]
    fn each_kind_of_nothing_says_what_actually_happened() {
        let empty = json!({"bpm": 0.0, "beats": [], "downbeats": [], "duration": 10.0});
        assert!(why(&empty, None).contains("found no beats"));
        let past = json!({"bpm": 0.0, "beats": [10.0, 11.0], "downbeats": [10.5], "duration": 10.0});
        assert!(why(&past, None).contains("lies outside"), "{}", why(&past, None));
        // Strings are garbage, not an absence of beats.
        let strings = json!({"bpm": 0.0, "beats": ["1.0", "2.0"], "downbeats": ["1.0"], "duration": 10.0});
        let w = why(&strings, None);
        assert!(w.contains("could not be read") && w.contains("3 entries") && !w.contains("no beats"), "{w}");
        // Neither field at all, no duration, a non-object.
        for v in [json!({"duration": 10.0}), json!({"beats": [1.0]}), json!({"beats": [1.0], "duration": -3.0}), json!("nonsense")] {
            let w = why(&v, None);
            assert!(w.contains("could not be read") && !w.contains("no beats"), "{v}: {w}");
        }
    }

    #[test]
    fn repairs_are_reported() {
        let v = json!({"bpm": 120.0,
            "beats": [1.0, 0.5, 0.5, 1.5, 12.0, "x"],
            "downbeats": [0.5, 10.2], "duration": 10.0});
        let (a, notes) = usable(&v);
        assert_eq!(a.beats, vec![0.5, 1.0, 1.5]);
        let all = notes.join(" | ");
        // Anchored: "beats" is a substring of "downbeats", so a bare `contains`
        // for a beats note would be satisfied by the downbeats one.
        for prefix in [
            "beats: 1 entries were not numbers",
            "beats: 1 timestamps outside",
            "beats: out of order, sorted",
            "beats: 1 duplicate",
            "downbeats: 1 timestamps outside",
        ] {
            assert!(notes.iter().any(|n| n.starts_with(prefix)), "no note starting {prefix:?} in {all}");
        }
        assert_eq!(notes.iter().filter(|n| n.starts_with("downbeats:")).count(), 1, "{all}");
    }

    #[test]
    fn an_analyser_that_measured_the_wrong_span_is_not_trusted() {
        // 30 s of beats for a 240 s track would switch for 30 s and then sit still.
        let v = json!({"bpm": 120.0, "beats": [1.0, 2.0, 3.0], "downbeats": [1.0], "duration": 30.0});
        let w = why(&v, Some(240.0));
        assert!(w.contains("30.0 s") && w.contains("240.0 s") && w.contains("wrong span"), "{w}");
        // Within a second is the container-vs-decoded difference and is fine.
        assert!(matches!(interpret(&v, Some(30.9)), Interpreted::Usable(..)));
        assert!(matches!(interpret(&v, Some(31.1)), Interpreted::Beatless(_)));
        // And an analyser that measured LONGER than the track, by the same rule.
        let long = json!({"bpm": 120.0, "beats": [1.0, 2.0, 3.0], "downbeats": [1.0], "duration": 300.0});
        let w = why(&long, Some(240.0));
        assert!(w.contains("300.0 s") && w.contains("240.0 s") && w.contains("wrong span"), "{w}");
        assert!(matches!(interpret(&v, Some(29.1)), Interpreted::Usable(..)));
        assert!(matches!(interpret(&v, Some(28.9)), Interpreted::Beatless(_)));
    }
}
