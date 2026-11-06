use projectmp4::audio::analysis::{analyse, analyse_outcome, DEFAULT_MODELS};
use projectmp4::audio::Source;
#[path = "common/tmp.rs"]
mod tmp;
use std::path::Path;

fn models() -> &'static Path {
    Path::new(DEFAULT_MODELS)
}

fn source_for(p: &str) -> Source {
    tmp::normalised(p)
}

#[test]
fn computes_from_scratch() {
    let src = source_for("samples/track.mp3");
    let a = analyse(&src, models()).unwrap();
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
    // ravezilla used to be answered from a sibling application's cache on the
    // maintainer's machine. It is analysed here, as it is everywhere.
    let a = analyse(&source_for("samples/ravezilla.m4a"), models()).unwrap();
    assert!(a.bpm > 40.0 && a.bpm < 220.0, "bpm {}", a.bpm);
    assert!(a.downbeats.len() < a.beats.len() && !a.downbeats.is_empty());
    assert!(a.downbeats.windows(2).all(|w| w[1] > w[0]));
    assert!(a.downbeats.iter().all(|t| *t < a.duration));
}

#[test]
fn missing_models_are_an_error_not_a_silent_fallback() {
    let src = source_for("samples/track.mp3");
    let e = analyse_outcome(&src, Path::new("/nonexistent/projectmp4-test-models"), None)
        .expect_err("an analyser that cannot run produced an outcome");
    assert!(e.to_string().contains("could not run"), "{e}");
}
