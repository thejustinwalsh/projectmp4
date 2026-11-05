use projectmp4::audio::analysis::{analyse, analyse_outcome_with_cache_dir, DEFAULT_MODELS};
use projectmp4::audio::{Analysis, Outcome};
#[path = "common/tmp.rs"]
mod tmp;
use tmp::Tmp;
use projectmp4::audio::Source;
use std::path::{Path, PathBuf};

const RAVEZILLA: &str = "samples/ravezilla.m4a";

fn tmpdir() -> Tmp {
    Tmp::new("analysis")
}

fn models() -> &'static Path {
    Path::new(DEFAULT_MODELS)
}

fn source_for(p: &str) -> Source {
    tmp::normalised(p)
}

/// The seam production runs: `analyse_outcome` calls this with the agent-deck
/// root and the decoded wav length. `track_secs` is `None` only where a test
/// has no wav length to offer.
fn cached(src: &Source, root: &Path, track_secs: Option<f64>) -> Analysis {
    match analyse_outcome_with_cache_dir(src, models(), root, track_secs).unwrap() {
        Outcome::Beats(a) => a,
        Outcome::NoBeats(why) => panic!("beat analysis: {why}"),
    }
}

fn real_cache_entry(sha: &str) -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap())
        .join("Library/Application Support/agent-deck/analysis")
        .join(format!("{sha}.json"))
}

/// Cache root in a private tmpdir with `contents` at analysis/<sha>.json.
fn cache_with(sha: &str, contents: &[u8]) -> Tmp {
    let root = tmpdir();
    std::fs::create_dir_all(root.join("analysis")).unwrap();
    std::fs::write(root.join("analysis").join(format!("{sha}.json")), contents).unwrap();
    root
}

#[test]
fn uses_agent_deck_cache_when_present() {
    // Environmental: requires the ravezilla entry to exist in the user's real agent-deck cache.
    let a = analyse(&source_for(RAVEZILLA), models()).unwrap();
    assert!(a.from_cache);
    assert!((a.bpm - 136.4).abs() < 0.5);
    assert!(!a.downbeats.is_empty());
    assert_eq!(a.beats.len(), 341);
    assert_eq!(a.downbeats.len(), 99);
    assert!((a.downbeats[0] - 1.58).abs() < 1e-6);
}

#[test]
fn computes_from_scratch_with_no_cache() {
    let src = source_for("samples/track.mp3");
    // Environmental: fails if the user's real agent-deck cache ever gains an entry for this file.
    assert!(!real_cache_entry(&src.sha256).exists(), "fixture must have no cache entry");
    let a = analyse(&src, models()).unwrap();
    assert!(!a.from_cache);
    assert!(a.bpm > 40.0 && a.bpm < 220.0);
    assert!(!a.beats.is_empty());
    assert!(!a.downbeats.is_empty());
    assert!(a.downbeats.len() < a.beats.len(), "downbeats are a subset of beats");
    assert!(a.beats.windows(2).all(|w| w[1] > w[0]), "beats must be ascending");
    assert!(a.downbeats.windows(2).all(|w| w[1] > w[0]), "downbeats must be ascending");
    assert!(a.downbeats.iter().all(|t| *t < a.duration));
}

#[test]
fn corrupt_cache_entry_falls_back_instead_of_failing() {
    let src = source_for(RAVEZILLA);
    // Environmental: requires the ravezilla entry to exist in the user's real agent-deck cache.
    let real = std::fs::read(real_cache_entry(&src.sha256)).unwrap();

    // Control: a valid entry at the path the code looks at is a cache hit.
    let good = cache_with(&src.sha256, &real);
    let hit = cached(&src, &good, None);
    assert!(hit.from_cache, "lookup must find a valid entry, else this test proves nothing");

    // Same path, garbage contents: must fall back and still produce a grid.
    let bad = cache_with(&src.sha256, b"{ not json \xff\xfe");
    let a = cached(&src, &bad, None);
    assert!(!a.from_cache);
    assert!(!a.beats.is_empty() && !a.downbeats.is_empty());
    assert!(a.downbeats.len() < a.beats.len());
    assert!(a.bpm > 40.0 && a.bpm < 220.0);
}

#[test]
fn unordered_downbeats_in_cache_fall_back() {
    let src = source_for(RAVEZILLA);
    let entry = |downbeats: [f64; 3]| {
        serde_json::to_vec(&serde_json::json!({
            "bpm": 120.0,
            "beats": [0.5, 1.0, 1.5, 2.0, 2.5, 3.0],
            "downbeats": downbeats,
            "duration": 100.0,
        }))
        .unwrap()
    };

    // Control: the same entry with ascending downbeats is a cache hit, so the
    // only defect of the bad entry below is its ordering.
    let good = cache_with(&src.sha256, &entry([0.5, 2.5, 4.5]));
    assert!(cached(&src, &good, None).from_cache);

    let bad = cache_with(&src.sha256, &entry([4.5, 2.5, 0.5]));
    let a = cached(&src, &bad, None);
    assert!(!a.from_cache, "descending downbeats must be rejected as a cache miss");
}

// --- the cache is read through the seam production uses, with the track's own length ---

/// Models that cannot exist: any path that reaches the analyser instead of the
/// cache fails loudly, so "was this a cache hit?" is observable without running it.
const NO_MODELS: &str = "/nonexistent/projectmp4-test-models";

fn outcome(src: &Source, root: &Path, track_secs: Option<f64>) -> projectmp4::ffi::Result<Outcome> {
    analyse_outcome_with_cache_dir(src, Path::new(NO_MODELS), root, track_secs)
}

fn entry(duration: f64, downbeats: serde_json::Value, beats: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"bpm": 120.0, "beats": beats, "downbeats": downbeats, "duration": duration})).unwrap()
}

#[test]
fn a_cached_duration_that_disagrees_with_the_track_is_a_miss_not_a_truncation() {
    let src = source_for(RAVEZILLA);
    // 120 s of downbeats for a 300 s track: honoured, the last three minutes sit on one preset.
    let downbeats = serde_json::json!([2.0, 10.0, 60.0, 110.0]);
    let root = cache_with(&src.sha256, &entry(120.0, downbeats.clone(), serde_json::json!([2.0, 10.0])));

    // Controls: the same entry is a hit for a track of its own length (within the
    // container-vs-decoded tolerance) and when no length is known.
    for secs in [Some(120.0), Some(120.9), None] {
        match outcome(&src, &root, secs).expect("control must be a cache hit") {
            Outcome::Beats(a) => assert!(a.from_cache && a.downbeats.len() == 4, "{secs:?}"),
            other => panic!("{secs:?}: {other:?}"),
        }
    }
    // The divergent lengths go to the analyser, which here cannot run: that error IS the miss.
    for secs in [300.0, 118.9, 121.1] {
        let e = outcome(&src, &root, Some(secs)).expect_err(&format!("{secs}: a divergent cache entry was used"));
        assert!(e.to_string().contains("could not run"), "{secs}: {e}");
    }
}

#[test]
fn a_cached_duration_longer_than_the_track_is_also_a_miss() {
    let src = source_for(RAVEZILLA);
    let root = cache_with(&src.sha256, &entry(600.0, serde_json::json!([2.0, 10.0]), serde_json::json!([2.0])));
    assert!(outcome(&src, &root, Some(300.0)).is_err());
}

#[test]
fn a_beatless_track_is_cached_and_does_not_reach_the_analyser() {
    let src = source_for(RAVEZILLA);
    let root = cache_with(&src.sha256, &entry(90.0, serde_json::json!([]), serde_json::json!([])));
    match outcome(&src, &root, Some(90.0)).expect("a cached 'no beats' must be a hit, not a re-analysis") {
        Outcome::NoBeats(why) => assert!(why.contains("no beats"), "{why}"),
        other => panic!("{other:?}"),
    }
    // Still subject to the duration check.
    assert!(outcome(&src, &root, Some(300.0)).is_err());
}

#[test]
fn damaged_beats_do_not_cost_the_cache_entry_but_damaged_downbeats_do() {
    let src = source_for(RAVEZILLA);
    let root = cache_with(&src.sha256, &entry(100.0, serde_json::json!([2.0, 4.0]), serde_json::json!([4.0, 2.0, 2.0, -1.0])));
    assert!(matches!(outcome(&src, &root, Some(100.0)), Ok(Outcome::Beats(a)) if a.from_cache && a.downbeats == [2.0, 4.0]));
    let root = cache_with(&src.sha256, &entry(100.0, serde_json::json!([4.0, 2.0]), serde_json::json!([2.0, 4.0])));
    assert!(outcome(&src, &root, Some(100.0)).is_err());
}
