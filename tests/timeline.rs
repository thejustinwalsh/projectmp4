use projectmp4::audio::analysis::{analyse, DEFAULT_MODELS};
use projectmp4::audio::timeline::{plan_switches, Cut};
use projectmp4::audio::normalise::PIPELINE_RATE;
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
    // No two switches are closer than drop_gap, so duration / drop_gap bounds the count above.
    // After a kept switch the next eligible downbeat arrives within about one downbeat period
    // of min_gap, so duration / (min_gap + 2 * mean spacing) bounds it below.
    let (min_gap, drop_gap) = (8.0, 4.0);
    let spacing = (a.downbeats.last().unwrap() - a.downbeats[0]) / (a.downbeats.len() - 1) as f64;
    let upper = (a.duration / drop_gap).floor() as usize;
    let lower = (a.duration / (min_gap + 2.0 * spacing)).floor() as usize;
    assert!(
        (lower..=upper).contains(&sw.len()),
        "switch count {} outside derived bounds {lower}..={upper}",
        sw.len()
    );
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
    let sw = plan_switches(&a, &d, 8.0, 4.0);
    assert!(sw.len() <= 2, "{} switches on a {:.1} s track with min_gap 8", sw.len(), a.duration);
    assert!(sw.iter().all(|s| s.t < a.duration));
    let (a, d) = fixture(THREE_NOTE_SIGNATURE);
    assert_eq!(plan_switches(&a, &d, 20.0, 20.0).len(), 0);
}

/// 40 s stereo track, downbeats every 2 s. A 60 Hz bed whose level changes bar to bar
/// (so the median downbeat step is non-zero), plus a 2 s `burst_hz` burst starting
/// exactly on the downbeat at `BURST_AT`.
const BURST_AT: f64 = 20.0;

fn synthetic(burst_hz: f64) -> (Analysis, Driver) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let tmp = std::env::temp_dir().join(format!(
        "projectmp4-timeline-synth-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let rate = PIPELINE_RATE;
    let total = 40 * rate as usize;
    let mut pcm = Vec::with_capacity(total * 4);
    for i in 0..total {
        let t = i as f64 / rate as f64;
        let bar = (t / 2.0) as usize;
        let bed = 0.05 + 0.03 * ((bar * 7) % 5) as f64 / 4.0;
        let mut v = bed * (2.0 * std::f64::consts::PI * 60.0 * t).sin();
        if (BURST_AT..BURST_AT + 2.0).contains(&t) {
            v += 0.4 * (2.0 * std::f64::consts::PI * burst_hz * t).sin();
        }
        let s = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
        pcm.extend_from_slice(&s.to_le_bytes());
        pcm.extend_from_slice(&s.to_le_bytes());
    }
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&2u16.to_le_bytes()); // stereo
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 4).to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    let path = tmp.join("synthetic.wav");
    std::fs::write(&path, wav).unwrap();
    let src = normalise(&path, &tmp).unwrap();
    let d = Driver::load(&src, Some(DriverKind::Mix)).unwrap();
    assert_eq!(d.rate, rate, "premise: driver runs at the pipeline rate");
    let a = Analysis {
        bpm: 120.0,
        beats: Vec::new(),
        downbeats: (1..20).map(|k| 2.0 * k as f64).collect(),
        duration: d.frames() as f64 / d.rate as f64,
        from_cache: false,
    };
    (a, d)
}

#[test]
fn low_frequency_burst_on_a_downbeat_is_a_hard_cut() {
    let (a, d) = synthetic(60.0);
    // Soft cuts land at 6, 12, 18; the downbeat at 20 is only 2 s later, so it can
    // only be kept as a drop.
    let sw = plan_switches(&a, &d, 6.0, 2.0);
    let at = sw.iter().find(|s| (s.t - BURST_AT).abs() < 1e-6);
    assert_eq!(at.map(|s| s.cut), Some(Cut::Hard), "no Hard cut at {BURST_AT}: {sw:?}");
    assert!(
        sw.iter().filter(|s| s.cut == Cut::Hard).all(|s| (s.t - BURST_AT).abs() < 1e-6),
        "Hard cut away from the drop: {sw:?}"
    );
}

#[test]
fn high_frequency_burst_of_equal_level_is_not_a_drop() {
    let (a, d) = synthetic(4000.0);
    let sw = plan_switches(&a, &d, 6.0, 2.0);
    assert!(
        sw.iter().all(|s| s.cut == Cut::Soft),
        "4 kHz burst was treated as a low-band drop: {sw:?}"
    );
    assert!(sw.iter().all(|s| (s.t - BURST_AT).abs() > 1e-6), "switch at {BURST_AT}: {sw:?}");
}
