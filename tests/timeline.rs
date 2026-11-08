use projectmp4::audio::analysis::{analyse, DEFAULT_MODELS};
use projectmp4::audio::timeline::{plan_switches, Cut};
use projectmp4::audio::normalise::PIPELINE_RATE;
#[path = "common/tmp.rs"]
mod tmp;
use tmp::Tmp;
use projectmp4::audio::{normalise, Analysis, Driver, Source};
use std::path::Path;

/// A generated click track standing in for a full-length song / a short motif /
/// a shorter signature. The switch bounds below are derived from the analysed
/// downbeat spacing, so they hold for any track, not just a particular one.
fn long_track() -> Source {
    tmp::click_source(60.0, 120.0)
}
fn motif() -> Source {
    tmp::click_source(16.0, 120.0)
}
fn signature() -> Source {
    tmp::click_source(12.0, 120.0)
}

fn fixture(src: Source) -> (Analysis, Driver) {
    let a = analyse(&src, Path::new(DEFAULT_MODELS)).unwrap();
    let d = Driver::load(&src).unwrap();
    (a, d)
}

#[test]
fn every_switch_lands_exactly_on_a_downbeat() {
    let (a, d) = fixture(long_track());
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
    let (a, d) = fixture(long_track());
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
    // About the music: a drop is a change in a real arrangement, and a click
    // track has none (every switch comes out Soft).
    let Some(src) = tmp::sample("samples/ravezilla.m4a") else { return };
    let (a, d) = fixture(src);
    let sw = plan_switches(&a, &d, 8.0, 4.0);
    assert!(sw.iter().any(|s| s.cut == Cut::Hard), "no Hard cut: drops are not detected");
    assert!(sw.iter().any(|s| s.cut == Cut::Soft), "no Soft cut: every switch was hard");
}

#[test]
fn track_shorter_than_min_gap_yields_no_switches() {
    let (a, d) = fixture(motif());
    let sw = plan_switches(&a, &d, 8.0, 4.0);
    assert!(sw.len() <= 2, "{} switches on a {:.1} s track with min_gap 8", sw.len(), a.duration);
    assert!(sw.iter().all(|s| s.t < a.duration));
    let (a, d) = fixture(signature());
    assert_eq!(plan_switches(&a, &d, 20.0, 20.0).len(), 0);
}

/// 40 s stereo track, downbeats every 2 s. A 60 Hz bed whose level changes bar to bar
/// (so the median downbeat step is non-zero), plus a 2 s `burst_hz` burst starting
/// exactly on the downbeat at `BURST_AT`.
const BURST_AT: f64 = 20.0;

fn synthetic(burst_hz: f64) -> (Analysis, Driver) {
    let d = driver_from(40.0, |t| {
        let bar = (t / 2.0) as usize;
        let bed = 0.05 + 0.03 * ((bar * 7) % 5) as f64 / 4.0;
        let mut v = bed * (2.0 * std::f64::consts::PI * 60.0 * t).sin();
        if (BURST_AT..BURST_AT + 2.0).contains(&t) {
            v += 0.4 * (2.0 * std::f64::consts::PI * burst_hz * t).sin();
        }
        v
    });
    let a = Analysis {
        bpm: 120.0,
        beats: Vec::new(),
        downbeats: (1..20).map(|k| 2.0 * k as f64).collect(),
        duration: d.frames() as f64 / d.rate as f64,
    };
    (a, d)
}

/// A `len_s` second stereo track whose sample at time `t` is `f(t)`, run through
/// the real `normalise` and `Driver::load`, so it is the pipeline's own signal.
fn driver_from(len_s: f64, f: impl Fn(f64) -> f64) -> Driver {
    let tmp = Tmp::new("timeline-synth");
    let rate = PIPELINE_RATE;
    let total = (len_s * rate as f64) as usize;
    let mut pcm = Vec::with_capacity(total * 4);
    for i in 0..total {
        let t = i as f64 / rate as f64;
        let v = f(t);
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
    let d = Driver::load(&src).unwrap();
    assert_eq!(d.rate, rate, "premise: driver runs at the pipeline rate");
    d
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
    // The exact list, not "all of them are Soft": that is true of an empty plan too.
    // 20 is absent because a drop at 20 would have been kept 2 s after the switch at 18.
    let got: Vec<(f64, Cut)> = sw.iter().map(|s| (s.t, s.cut)).collect();
    let want: Vec<(f64, Cut)> = [6.0, 12.0, 18.0, 24.0, 30.0, 36.0].iter().map(|t| (*t, Cut::Soft)).collect();
    assert_eq!(got, want, "4 kHz burst must neither become a drop nor disturb the Soft schedule");
}

// --- the tracks that used to render on one preset ---------------------------------

use projectmp4::audio::timeline::{fill_gaps, plan_switches_reporting};
use std::sync::OnceLock;

fn sine(hz: f64, t: f64) -> f64 {
    (2.0 * std::f64::consts::PI * hz * t).sin()
}

/// 150 s: a quiet bed for the first 90 s, then a louder one that changes bar to bar.
fn ambient_intro() -> &'static Driver {
    static D: OnceLock<Driver> = OnceLock::new();
    D.get_or_init(|| {
        driver_from(150.0, |t| {
            let bed = if t < 90.0 { 0.02 } else { 0.1 + 0.05 * (((t / 2.0) as usize * 7) % 5) as f64 / 4.0 };
            bed * sine(60.0, t)
        })
    })
}

fn analysis(downbeats: Vec<f64>, d: &Driver) -> Analysis {
    Analysis { bpm: 0.0, beats: downbeats.clone(), downbeats, duration: d.frames() as f64 / d.rate as f64 }
}

/// Longest stretch with no preset change, counting from the opening preset at 0 to the end.
fn longest_still(sw: &[projectmp4::audio::timeline::SwitchPoint], duration: f64) -> f64 {
    let mut prev = 0.0;
    let mut worst: f64 = 0.0;
    for s in sw {
        worst = worst.max(s.t - prev);
        prev = s.t;
    }
    worst.max(duration - prev)
}

#[test]
fn a_ninety_second_quiet_intro_keeps_switching_and_says_so() {
    let d = ambient_intro();
    // The analyser found bar lines only from 1:30.
    let a = analysis((45..75).map(|k| 2.0 * k as f64).collect(), d);
    let (sw, fill) = plan_switches_reporting(&a, d, 8.0, 4.0);
    let before_first_downbeat = sw.iter().filter(|s| s.t < 90.0).count();
    assert!(before_first_downbeat >= 9, "the 90 s intro got {before_first_downbeat} switches: {sw:?}");
    let still = longest_still(&sw, a.duration);
    assert!(still <= 16.0 + 1e-9, "{still} s on one preset: {sw:?}");
    let notice = fill.notice(a.duration).expect("a grid in place of the music must be reported");
    assert!(notice.contains("90 s") && notice.contains("150 s") && notice.contains("fixed 8 s interval"), "{notice}");
}

#[test]
fn an_outro_after_the_last_downbeat_keeps_switching() {
    let d = ambient_intro();
    // Bar lines stop at 40 s of a 150 s track.
    let a = analysis((1..=20).map(|k| 2.0 * k as f64).collect(), d);
    let (sw, fill) = plan_switches_reporting(&a, d, 8.0, 4.0);
    assert!(longest_still(&sw, a.duration) <= 16.0 + 1e-9, "the tail sat still: {sw:?}");
    assert!(sw.last().unwrap().t > 130.0, "{sw:?}");
    assert_eq!(fill.secs.round() as i64, 110, "{fill:?}");
}

#[test]
fn a_single_detected_beat_is_no_worse_than_none() {
    let d = ambient_intro();
    let a = analysis(vec![3.0], d);
    let (sw, fill) = plan_switches_reporting(&a, d, 8.0, 4.0);
    assert!(sw.len() >= 15, "one beat froze the track: {} switches", sw.len());
    assert!(longest_still(&sw, a.duration) <= 16.0 + 1e-9, "{sw:?}");
    assert!(fill.notice(a.duration).is_some(), "silent degradation");
    // And the same track with no beats at all is the same grid, not something better.
    let none = analysis(vec![], d);
    let (sw0, _) = plan_switches_reporting(&none, d, 8.0, 4.0);
    assert_eq!(sw, sw0, "one beat must fall back exactly as zero does");
    // Too short for the `2 * grid` rule, but a track with nothing to go on still gets its grid.
    let (pts, _) = fill_gaps(&[3.0], 14.0, 8.0);
    assert_eq!(pts, [8.0]);
}

#[test]
fn a_gap_inside_a_beated_track_is_filled_and_a_complete_schedule_is_untouched() {
    let d = ambient_intro();
    // Bar lines at 2..40, a 60 s breakdown, then 100..148.
    let mut db: Vec<f64> = (1..=20).map(|k| 2.0 * k as f64).collect();
    db.extend((50..=74).map(|k| 2.0 * k as f64));
    let a = analysis(db, d);
    let (sw, fill) = plan_switches_reporting(&a, d, 8.0, 4.0);
    assert!(longest_still(&sw, a.duration) <= 16.0 + 1e-9, "{sw:?}");
    assert_eq!(fill.secs.round() as i64, 60, "{fill:?}");

    let whole = analysis((1..75).map(|k| 2.0 * k as f64).collect(), d);
    let (_, fill) = plan_switches_reporting(&whole, d, 8.0, 4.0);
    assert_eq!((fill.added, fill.notice(whole.duration)), (0, None), "the music supplied every point");
}

#[test]
fn grid_points_keep_clear_of_the_real_downbeat_that_ends_the_gap() {
    let (pts, fill) = fill_gaps(&[90.0, 92.0], 150.0, 8.0);
    for g in pts.iter().filter(|t| ![90.0, 92.0].contains(t)) {
        assert!([90.0f64, 92.0].iter().all(|r| (g - r).abs() >= 4.0 - 1e-9), "grid point {g} crowds a real downbeat: {pts:?}");
    }
    assert!(pts.contains(&90.0) && pts.contains(&92.0) && pts.iter().all(|t| *t < 150.0));
    assert!(fill.added > 0);
    // Degenerate input must not loop or panic.
    assert!(fill_gaps(&[], 100.0, 0.0).0.is_empty());
    assert!(fill_gaps(&[], f64::NAN, 8.0).0.is_empty());
}

#[test]
fn a_majority_silent_intro_does_not_disable_the_drop_cut() {
    // 40 s, digital silence until 24 s (11 of the 19 downbeats have a step of exactly 0),
    // then a bed that moves bar to bar, and a low-frequency burst on the downbeat at 34.
    let d = driver_from(40.0, |t| {
        if t < 24.0 {
            return 0.0;
        }
        let bar = (t / 2.0) as usize;
        let bed = 0.05 + 0.03 * ((bar * 7) % 5) as f64 / 4.0;
        let burst = if (34.0..36.0).contains(&t) { 0.4 } else { 0.0 };
        (bed + burst) * sine(60.0, t)
    });
    let a = Analysis {
        bpm: 120.0,
        beats: Vec::new(),
        downbeats: (1..20).map(|k| 2.0 * k as f64).collect(),
        duration: d.frames() as f64 / d.rate as f64,
    };
    let sw = plan_switches(&a, &d, 6.0, 2.0);
    let at = sw.iter().find(|s| (s.t - 34.0).abs() < 1e-6);
    assert_eq!(at.map(|s| s.cut), Some(Cut::Hard), "the drop at 34 got no Hard cut: {sw:?}");
    // The burst at 34 is the only drop. A zero threshold would also make every positive
    // bar-to-bar wobble of the bed a Hard cut.
    let hard: Vec<f64> = sw.iter().filter(|s| s.cut == Cut::Hard).map(|s| s.t).collect();
    assert_eq!(hard, [34.0], "{sw:?}");
}

#[test]
fn a_downbeat_too_near_the_start_does_not_calibrate_the_drop_threshold() {
    // Silent for the first 0.1 s, then a steady bed: a downbeat at 0.12 sees a
    // 0.12 s "before" that is mostly silence, a huge and meaningless step. The
    // drop at 24 is smaller than that step, so if the junk step enters the
    // median the drop's own threshold climbs above it.
    let d = driver_from(40.0, |t| {
        if t < 0.1 {
            return 0.0;
        }
        let amp = if (24.0..26.0).contains(&t) { 0.6 } else { 0.5 };
        amp * sine(60.0, t)
    });
    let a = Analysis {
        bpm: 120.0,
        beats: Vec::new(),
        downbeats: vec![0.12, 12.0, 24.0, 36.0],
        duration: d.frames() as f64 / d.rate as f64,
    };
    let sw = plan_switches(&a, &d, 8.0, 4.0);
    let got: Vec<(f64, Cut)> = sw.iter().map(|s| (s.t, s.cut)).collect();
    assert_eq!(got, [(12.0, Cut::Soft), (24.0, Cut::Hard), (36.0, Cut::Soft)], "{sw:?}");
}

// --- the grid notice must be worth reading ---------------------------------------------

fn notice_for(last_downbeat: f64) -> Option<String> {
    let d = ambient_intro(); // 150 s: the 20% line is 30 s of grid
    let a = analysis((1..).map(|k| 2.0 * k as f64).take_while(|t| *t <= last_downbeat).collect(), d);
    plan_switches_reporting(&a, d, 8.0, 4.0).1.notice(a.duration)
}

#[test]
fn an_ordinary_outro_prints_nothing() {
    // Grid covers the 28 s after the last bar line: under 20% of 150 s, under 60 s.
    assert_eq!(notice_for(122.0), None);
    // And a typical short outro.
    assert_eq!(notice_for(140.0), None);
}

#[test]
fn a_long_stretch_on_the_grid_prints_how_much() {
    // 32 s of 150 s is over 20% (30 s).
    let n = notice_for(118.0).expect("over 20% of the track on the grid must be reported");
    assert!(n.contains("32 s of this 150 s track") && n.contains("fixed 8 s interval"), "{n}");
    // The 60 s line, on a track long enough that 20% is further away: 59 s is silent, 60 s speaks.
    let g = |secs| projectmp4::audio::timeline::GridFill { added: 7, secs, grid: 8.0 };
    assert_eq!(g(59.0).notice(600.0), None);
    assert!(g(60.0).notice(600.0).is_some());
    // 20% line exactly, short track.
    assert_eq!(g(29.9).notice(150.0), None);
    assert!(g(30.0).notice(150.0).is_some());
    // No downbeats at all is 100%.
    let d = ambient_intro();
    let (_, fill) = plan_switches_reporting(&analysis(vec![], d), d, 8.0, 4.0);
    assert!(fill.notice(150.0).is_some());
}

#[test]
fn the_notice_reports_the_grid_that_was_used_not_one_the_caller_names() {
    let d = ambient_intro();
    // A 6 s grid: the notice must say 6, whatever any caller might assume of 8.
    let (_, fill) = plan_switches_reporting(&analysis(vec![], d), d, 6.0, 3.0);
    let n = fill.notice(150.0).unwrap();
    assert!(n.contains("fixed 6 s interval") && !n.contains("fixed 8 s"), "{n}");
}
