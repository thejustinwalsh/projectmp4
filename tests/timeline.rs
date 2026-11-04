use projectmp4::audio::analysis::{analyse, DEFAULT_MODELS};
use projectmp4::audio::timeline::{plan_switches, Cut};
use projectmp4::audio::{normalise, Analysis, Driver, DriverKind};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

const RAVEZILLA: &str = "samples/ravezilla.m4a";
const THREE_NOTE_MOTIF: &str = "samples/three-note-motif.m4a";
const THREE_NOTE_SIGNATURE: &str = "samples/three-note-signature.m4a";

fn fixture(p: &str) -> (Analysis, Driver) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let tmp = std::env::temp_dir().join(format!(
        "projectmp4-timeline-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let src = normalise(Path::new(p), &tmp).unwrap();
    let a = analyse(&src, Path::new(DEFAULT_MODELS)).unwrap();
    let d = Driver::load(&src, Some(DriverKind::Mix)).unwrap();
    (a, d)
}

#[test]
fn every_switch_lands_exactly_on_a_downbeat() {
    let (a, d) = fixture(RAVEZILLA);
    let sw = plan_switches(&a, &d, 8.0, 4.0);
    assert!((5..=21).contains(&sw.len()), "implausible switch count {}", sw.len());
    for s in sw {
        assert!(a.downbeats.iter().any(|db| (db - s.t).abs() < 1e-6), "{} is not a downbeat", s.t);
    }
}

#[test]
fn respects_the_minimum_gap_except_at_drops() {
    let (a, d) = fixture(RAVEZILLA);
    let sw = plan_switches(&a, &d, 8.0, 4.0);
    assert!(sw.len() >= 2, "need at least two switches, got {}", sw.len());
    // The opening preset counts as a switch at t = 0, so the first gap is real.
    let mut prev = 0.0;
    for s in &sw {
        let gap = s.t - prev;
        assert!(gap >= 4.0 - 1e-9, "gap {gap} before {} below drop_gap", s.t);
        if s.cut == Cut::Soft {
            assert!(gap >= 8.0 - 1e-9, "soft gap {gap} before {} below min_gap", s.t);
        }
        prev = s.t;
    }
}

#[test]
fn drops_get_hard_cuts() {
    let (a, d) = fixture(RAVEZILLA);
    let sw = plan_switches(&a, &d, 8.0, 4.0);
    assert!(sw.iter().any(|s| s.cut == Cut::Hard), "no Hard cut: drops are not detected");
    assert!(sw.iter().any(|s| s.cut == Cut::Soft), "no Soft cut: every switch was hard");
}

#[test]
fn track_shorter_than_min_gap_yields_no_switches() {
    let (a, d) = fixture(THREE_NOTE_MOTIF);
    assert!(plan_switches(&a, &d, 8.0, 4.0).iter().all(|s| s.t < a.duration));
    let (a, d) = fixture(THREE_NOTE_SIGNATURE);
    assert_eq!(plan_switches(&a, &d, 20.0, 20.0).len(), 0);
}
