use projectmp4::audio::analysis::{analyse, analyse_with_cache_dir, DEFAULT_MODELS};
use projectmp4::audio::{normalise, Source};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const RAVEZILLA: &str = "samples/ravezilla.m4a";

fn tmpdir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "projectmp4-analysis-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn models() -> &'static Path {
    Path::new(DEFAULT_MODELS)
}

fn source_for(p: &str) -> Source {
    normalise(Path::new(p), &tmpdir()).unwrap()
}

fn real_cache_entry(sha: &str) -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap())
        .join("Library/Application Support/agent-deck/analysis")
        .join(format!("{sha}.json"))
}

/// Cache root in a private tmpdir with `contents` at analysis/<sha>.json.
fn cache_with(sha: &str, contents: &[u8]) -> PathBuf {
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
    let hit = analyse_with_cache_dir(&src, models(), &good).unwrap();
    assert!(hit.from_cache, "lookup must find a valid entry, else this test proves nothing");

    // Same path, garbage contents: must fall back and still produce a grid.
    let bad = cache_with(&src.sha256, b"{ not json \xff\xfe");
    let a = analyse_with_cache_dir(&src, models(), &bad).unwrap();
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
    assert!(analyse_with_cache_dir(&src, models(), &good).unwrap().from_cache);

    let bad = cache_with(&src.sha256, &entry([4.5, 2.5, 0.5]));
    let a = analyse_with_cache_dir(&src, models(), &bad).unwrap();
    assert!(!a.from_cache, "descending downbeats must be rejected as a cache miss");
}
