use projectmp4::audio::analysis::{analyse, analyse_outcome, DEFAULT_MODELS};
#[path = "common/tmp.rs"]
mod tmp;
use std::path::Path;

fn models() -> &'static Path {
    Path::new(DEFAULT_MODELS)
}

#[test]
fn computes_from_scratch() {
    let a = analyse(&tmp::click_source(30.0, 120.0), models()).unwrap();
    assert!(a.bpm > 40.0 && a.bpm < 220.0);
    assert!(!a.beats.is_empty());
    assert!(!a.downbeats.is_empty());
    assert!(a.downbeats.len() < a.beats.len(), "downbeats are a subset of beats");
    assert!(a.beats.windows(2).all(|w| w[1] > w[0]), "beats must be ascending");
    assert!(a.downbeats.windows(2).all(|w| w[1] > w[0]), "downbeats must be ascending");
    assert!(a.downbeats.iter().all(|t| *t < a.duration));
}

#[test]
fn a_second_track_is_analysed_the_same_way() {
    // A second track is not measured against the first: it gets its own tempo,
    // not a repeat of one already seen. The two fixtures differ only in tempo,
    // which is what makes a carried-over answer visible.
    let first = analyse(&tmp::click_source(30.0, 120.0), models()).unwrap();
    let a = analyse(&tmp::click_source(30.0, 150.0), models()).unwrap();
    assert!(a.bpm > 40.0 && a.bpm < 220.0, "bpm {}", a.bpm);
    assert!((a.bpm - first.bpm).abs() > 1.0, "second track reported the first's tempo, {}", a.bpm);
    assert!(a.downbeats.len() < a.beats.len() && !a.downbeats.is_empty());
    assert!(a.downbeats.windows(2).all(|w| w[1] > w[0]));
    assert!(a.downbeats.iter().all(|t| *t < a.duration));
}

#[test]
fn a_click_track_recovers_its_own_tempo() {
    // Ground truth is known, so the tempo is exact rather than "somewhere in
    // 40..220". Only these two tempos are measured as exact; `click_file` says why.
    for bpm in [120.0, 150.0] {
        let a = analyse(&tmp::click_source(30.0, bpm), models()).unwrap();
        assert!((a.bpm as f64 - bpm).abs() < 0.01, "asked for {bpm}, got {}", a.bpm);
        assert!(!a.beats.is_empty() && a.beats.windows(2).all(|w| w[1] > w[0]), "beats ascending");
        assert!(a.beats.iter().all(|t| *t < a.duration));
        assert!(a.downbeats.windows(2).all(|w| w[1] > w[0]) && a.downbeats.iter().all(|t| *t < a.duration));
    }
}

#[test]
fn missing_models_are_an_error_not_a_silent_fallback() {
    let src = tmp::click_source(30.0, 120.0);
    let e = analyse_outcome(&src, Path::new("/nonexistent/projectmp4-test-models"), None)
        .expect_err("an analyser that cannot run produced an outcome");
    assert!(e.to_string().contains("could not run"), "{e}");
}
