//! Clip selection (`--clip drop|intro`) and the WAV cut it makes. Everything
//! here is synthetic: no stems, no section model, nothing from `samples/`.

#[path = "common/tmp.rs"]
mod tmp;

use projectmp4::audio::normalise::{trim, wav_frames, Source, PIPELINE_RATE};
use projectmp4::audio::sections::clip::{choose, section_energy, ClipKind, Reason};
use projectmp4::audio::sections::{Section, LABELS};
use projectmp4::encode::{video_bitrate_kbps, Target, AUDIO_KBPS, X_MAX_SECONDS};
use projectmp4::render::plan::{RenderPlan, X_TARGET_WANT};
use tmp::Tmp;

const INTRO: u32 = 2;
const OUTRO: u32 = 3;
const VERSE: u32 = 8;
const CHORUS: u32 = 9;

fn sec(start: f64, end: f64, label: u32, confidence: f32) -> Section {
    Section { start, end, label, confidence }
}

/// A bar every `bar` seconds from 0 to just under `duration`.
fn grid(bar: f64, duration: f64) -> Vec<f64> {
    (0..).map(|i| i as f64 * bar).take_while(|t| *t < duration).collect()
}

#[test]
fn the_drop_clip_starts_on_a_downbeat_inside_the_best_chorus() {
    let downbeats = grid(2.0, 200.0);
    // The earlier chorus is the weaker one; its start (33.0) is off the grid.
    let sections = [
        sec(0.0, 20.0, INTRO, 0.9),
        sec(20.0, 33.0, VERSE, 0.9),
        sec(33.0, 63.0, CHORUS, 0.4),
        sec(63.0, 101.0, VERSE, 0.9),
        sec(101.0, 131.0, CHORUS, 0.8),
        sec(131.0, 160.0, OUTRO, 0.9),
    ];
    let energy = vec![0.1; sections.len()];
    let want = 20.0;
    let c = choose(&sections, &downbeats, &energy, Some(want), 160.0, ClipKind::Drop).unwrap();
    assert_eq!(c.reason, Reason::Chorus);
    assert_eq!(c.label, Some(CHORUS));
    assert!(downbeats.contains(&c.start), "start {} is not a downbeat", c.start);
    assert!((101.0..131.0).contains(&c.start), "start {} is outside the better chorus", c.start);
    let len = c.end - c.start;
    assert!(len <= want && len > want - 2.0, "length {len} is not within one bar below {want}");
    assert!(downbeats.contains(&c.end), "end {} is not a downbeat", c.end);
}

#[test]
fn a_track_with_no_chorus_falls_back_to_the_loudest_section_and_says_so() {
    let sections = [
        sec(0.0, 30.0, VERSE, 0.9),   // highest confidence, quiet
        sec(30.0, 60.0, 4, 0.3),      // break, the loudest eligible
        sec(60.0, 90.0, VERSE, 0.5),
        sec(90.0, 120.0, OUTRO, 0.9), // loudest of all, but an outro
    ];
    let energy = [0.2, 0.7, 0.4, 0.95];
    let c = choose(&sections, &grid(2.0, 120.0), &energy, Some(10.0), 120.0, ClipKind::Drop).unwrap();
    assert_eq!(c.reason, Reason::LoudestSection);
    assert_eq!(c.label, Some(4), "label must name the break, not {:?}", c.label.map(|l| LABELS[l as usize]));
    assert!((30.0..60.0).contains(&c.start));
}

#[test]
fn a_want_longer_than_the_track_returns_the_whole_track() {
    let sections = [sec(0.0, 40.0, CHORUS, 0.9)];
    let c = choose(&sections, &grid(2.0, 40.0), &[0.5], Some(41.0), 40.0, ClipKind::Drop).unwrap();
    assert_eq!((c.start, c.end), (0.0, 40.0));
    assert_eq!(c.reason, Reason::WholeTrack);
    assert_eq!(c.label, None);
}

#[test]
fn a_want_of_zero_or_a_negative_want_is_an_error() {
    let sections = [sec(0.0, 40.0, CHORUS, 0.9)];
    for w in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let e = choose(&sections, &[], &[0.5], Some(w), 40.0, ClipKind::Drop).unwrap_err().to_string();
        assert!(e.contains(&format!("{w}")), "error for {w} does not name it: {e}");
    }
}

#[test]
fn x_rejects_nothing_we_would_now_produce() {
    for bpm in (20..=60).step_by(5) {
        let bar = 4.0 * 60.0 / bpm as f64; // 4 s at 60 bpm, 12 s at 20
        let duration = 400.0;
        let downbeats = grid(bar, duration);
        for chorus_at in [0.0f64, 7.3, 50.0, 120.0, 233.3, 262.0] {
            let sections = [
                sec(0.0, chorus_at.max(1.0), VERSE, 0.5),
                sec(chorus_at.max(1.0), chorus_at.max(1.0) + 40.0, CHORUS, 0.9),
            ];
            let c = choose(&sections, &downbeats, &[0.1, 0.2], Some(X_TARGET_WANT), duration, ClipKind::Drop).unwrap();
            let len = c.end - c.start;
            assert!(len <= X_MAX_SECONDS, "bpm {bpm}, chorus at {chorus_at}: {len} s exceeds {X_MAX_SECONDS}");
            assert!(len <= X_TARGET_WANT, "bpm {bpm}, chorus at {chorus_at}: {len} s exceeds want");
            video_bitrate_kbps(Target::X, len, AUDIO_KBPS)
                .unwrap_or_else(|e| panic!("bpm {bpm}, chorus at {chorus_at}: {len} s refused: {e}"));
        }
    }
}

#[test]
fn a_beatless_track_gets_the_unsnapped_span() {
    let sections = [sec(10.3, 50.0, CHORUS, 0.9)];
    let c = choose(&sections, &[], &[0.5], Some(20.0), 100.0, ClipKind::Drop).unwrap();
    assert_eq!((c.start, c.end), (10.3, 30.3));
}

#[test]
fn want_defaults_to_the_sections_own_length() {
    let sections = [sec(10.0, 40.0, CHORUS, 0.9)];
    let c = choose(&sections, &grid(2.0, 100.0), &[0.5], None, 100.0, ClipKind::Drop).unwrap();
    assert_eq!((c.start, c.end), (10.0, 40.0));
}

#[test]
fn the_intro_clip_takes_the_intro_else_the_first_section() {
    let with = [sec(0.0, 10.0, VERSE, 0.9), sec(10.0, 30.0, INTRO, 0.5), sec(30.0, 90.0, CHORUS, 0.9)];
    let c = choose(&with, &[], &[0.1; 3], None, 90.0, ClipKind::Intro).unwrap();
    assert_eq!((c.start, c.end, c.reason), (10.0, 30.0, Reason::Intro));
    let without = [sec(0.0, 25.0, VERSE, 0.9), sec(25.0, 90.0, CHORUS, 0.9)];
    let c = choose(&without, &[], &[0.1; 2], None, 90.0, ClipKind::Intro).unwrap();
    assert_eq!((c.start, c.end, c.reason, c.label), (0.0, 25.0, Reason::FirstSection, Some(VERSE)));
}

#[test]
fn no_sections_and_the_full_kind_give_the_whole_track() {
    let c = choose(&[], &[], &[], Some(10.0), 90.0, ClipKind::Drop).unwrap();
    assert_eq!((c.start, c.end, c.reason, c.label), (0.0, 90.0, Reason::WholeTrack, None));
    let sections = [sec(0.0, 10.0, CHORUS, 0.9)];
    let c = choose(&sections, &[], &[0.1], Some(5.0), 90.0, ClipKind::Full).unwrap();
    assert_eq!((c.start, c.end, c.reason), (0.0, 90.0, Reason::WholeTrack));
}

#[test]
fn choose_refuses_inconsistent_input() {
    let sections = [sec(0.0, 10.0, CHORUS, 0.9)];
    let e = choose(&sections, &[], &[], None, 90.0, ClipKind::Drop).unwrap_err().to_string();
    assert!(e.contains('0') && e.contains('1'), "{e}");
    for bad in [sec(10.0, 10.0, CHORUS, 0.9), sec(10.0, 5.0, CHORUS, 0.9), sec(0.0, f64::NAN, CHORUS, 0.9)] {
        let e = choose(std::slice::from_ref(&bad), &[], &[0.1], None, 90.0, ClipKind::Drop).unwrap_err().to_string();
        assert!(e.contains(&format!("{}", bad.end)), "{e}");
    }
    for d in [0.0, -3.0, f64::NAN] {
        let e = choose(&sections, &[], &[0.1], None, d, ClipKind::Drop).unwrap_err().to_string();
        assert!(e.contains(&format!("{d}")), "{e}");
    }
}

#[test]
fn section_energy_scores_loud_above_quiet() {
    let sr = 1000u32;
    let mut mono = vec![0.05f32; 2000];
    mono[1000..].fill(0.5);
    let sections = [sec(0.0, 1.0, VERSE, 0.5), sec(1.0, 2.0, CHORUS, 0.5), sec(5.0, 6.0, VERSE, 0.5)];
    let e = section_energy(&sections, &mono, sr).unwrap();
    assert_eq!(e.len(), 3);
    assert!((e[0] - 0.05).abs() < 1e-6 && (e[1] - 0.5).abs() < 1e-6, "{e:?}");
    assert!(e[1] > e[0]);
    assert_eq!(e[2], 0.0, "a span past the end of the input scores zero");
    assert!(section_energy(&sections, &mono, 0).is_err());
}

#[test]
fn clip_kind_round_trips_and_names_its_choices_on_error() {
    for k in [ClipKind::Full, ClipKind::Drop, ClipKind::Intro] {
        assert_eq!(ClipKind::parse(k.name()).unwrap(), k);
    }
    assert_eq!(ClipKind::default(), ClipKind::Full);
    let e = ClipKind::parse("verse").unwrap_err().to_string();
    for w in ["verse", "full", "drop", "intro"] {
        assert!(e.contains(w), "{e}");
    }
}

// ---- trim ----

/// A normalised-format WAV (48 kHz stereo f32) of `frames` frames at constant
/// amplitude 1.0 on both channels, behind a `LIST` chunk so the data chunk is
/// not at byte 44.
fn flat_wav(dir: &std::path::Path, frames: usize) -> Source {
    let data_len = (frames * 8) as u32;
    let list = b"INFOISFT\x05\0\0\0Lavf\0\0"; // 8-byte INFO + ISFT chunk (13 -> padded 14)
    let list_len = list.len() as u32;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(4 + 24 + 8 + list_len + 8 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVE");
    b.extend_from_slice(b"fmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&3u16.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&PIPELINE_RATE.to_le_bytes());
    b.extend_from_slice(&(PIPELINE_RATE * 8).to_le_bytes());
    b.extend_from_slice(&8u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(b"LIST");
    b.extend_from_slice(&list_len.to_le_bytes());
    b.extend_from_slice(list);
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for _ in 0..frames * 2 {
        b.extend_from_slice(&1.0f32.to_le_bytes());
    }
    let wav = dir.join("flat.wav");
    std::fs::write(&wav, b).unwrap();
    Source {
        wav,
        sha256: "ab".repeat(32),
        duration: frames as f64 / PIPELINE_RATE as f64,
        title: Some("T".into()),
        artist: Some("A".into()),
    }
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn read_f32(path: &std::path::Path) -> Vec<f32> {
    let b = std::fs::read(path).unwrap();
    assert_eq!(&b[36..40], b"data", "trim writes a canonical 44-byte header");
    b[44..].chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

#[test]
fn trim_cuts_the_span_and_fades_both_real_edges() {
    let dir = Tmp::new("clip-trim");
    let src = flat_wav(&dir, 10 * PIPELINE_RATE as usize);
    let out = trim(&src, 2.0, 6.0, 0.5, &dir).unwrap();
    let (s0, s1) = (2 * PIPELINE_RATE as u64, 6 * PIPELINE_RATE as u64);
    assert_eq!(wav_frames(&out.wav).unwrap(), s1 - s0);
    assert_eq!(out.duration, 4.0);
    assert_eq!((out.sha256.as_str(), out.title.as_deref(), out.artist.as_deref()), (src.sha256.as_str(), Some("T"), Some("A")));
    assert_eq!(out.wav, dir.join(format!("{}-clip-{s0}-{s1}.wav", src.sha256)));
    assert!(!dir.join(format!("{}-clip-{s0}-{s1}.wav.part", src.sha256)).exists());

    let s = read_f32(&out.wav);
    let n = s.len() / 2;
    assert!(s[0].abs() < 1e-3 && s[1].abs() < 1e-3, "first frame {} not near zero", s[0]);
    assert!(s[2 * (n - 1)].abs() < 1e-3, "last frame {} not near zero", s[2 * (n - 1)]);
    assert_eq!(s[2 * (n / 2)], 1.0, "the middle is untouched");
    assert_eq!(s[2 * (n / 2)], s[2 * (n / 2) + 1]);
    // Half-way through the fade-in the raised cosine is 0.5.
    let f = (0.5 * PIPELINE_RATE as f64) as usize;
    assert!((s[2 * (f / 2)] - 0.5).abs() < 1e-3, "{}", s[2 * (f / 2)]);
}

#[test]
fn trim_does_not_fade_an_edge_that_is_not_a_cut() {
    let dir = Tmp::new("clip-trim-edge");
    let frames = 4 * PIPELINE_RATE as usize;
    let src = flat_wav(&dir, frames);
    let head = read_f32(&trim(&src, 0.0, 2.0, 0.5, &dir).unwrap().wav);
    assert_eq!(head[0], 1.0, "start 0.0 is not a cut");
    assert!(head[head.len() - 2].abs() < 1e-3, "but the end is");
    let tail = read_f32(&trim(&src, 2.0, 4.0, 0.5, &dir).unwrap().wav);
    assert!(tail[0].abs() < 1e-3);
    assert_eq!(tail[tail.len() - 2], 1.0, "end at the last sample is not a cut");
    // An end past the source clamps, and is then not a cut either.
    let past = read_f32(&trim(&src, 2.0, 99.0, 0.5, &dir).unwrap().wav);
    assert_eq!(past.len() / 2, frames / 2);
    assert_eq!(past[past.len() - 2], 1.0);
}

#[test]
fn trim_with_a_fade_longer_than_the_clip_is_not_silent() {
    let dir = Tmp::new("clip-trim-long-fade");
    let src = flat_wav(&dir, 4 * PIPELINE_RATE as usize);
    let s = read_f32(&trim(&src, 1.0, 2.0, 1.5, &dir).unwrap().wav);
    let peak = s.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(peak > 0.99, "peak {peak}");
}

#[test]
fn trim_refuses_bad_spans_naming_the_values() {
    let dir = Tmp::new("clip-trim-bad");
    let src = flat_wav(&dir, 2 * PIPELINE_RATE as usize);
    // (start, end, fade, the one value the error must name)
    let rows: [(f64, f64, f64, String); 8] = [
        (f64::NAN, 1.0, 0.1, "NaN".into()),
        (0.0, f64::INFINITY, 0.1, "inf".into()),
        (0.0, 1.0, f64::NAN, "NaN".into()),
        (0.0, 1.0, -0.5, "-0.5".into()),
        (-1.0, 1.0, 0.1, "-1".into()),
        (1.0, 1.0, 0.1, "1..1".into()),
        (1.5, 1.0, 0.1, "1.5..1".into()),
        (5.0, 6.0, 0.1, "5..6".into()),
    ];
    for (s, e, f, named) in rows {
        let err = trim(&src, s, e, f, &dir).unwrap_err().to_string();
        assert!(err.contains(&named), "({s},{e},{f}) must name {named:?}: {err}");
    }
    assert!(std::fs::read_dir(&*dir).unwrap().filter_map(|e| e.ok()).all(|e| e.path().extension().is_none_or(|x| x != "part")));
}

// ---- plan JSON ----

fn plan() -> RenderPlan {
    RenderPlan::from_json(
        r#"{"input":"a.mp3","title":"a","fps":60,"duration":10.0,"total_frames":600,"bpm":120.0,
            "switches":[],"preset_sequence":["x.milk"],"pool":["x.milk"]}"#,
    )
    .unwrap()
}

#[test]
fn a_plan_with_no_clip_fields_loads_as_full() {
    let p = plan();
    assert_eq!(p.clip, ClipKind::Full);
    assert_eq!((p.clip_span, p.clip_reason.clone(), p.clip_label.clone()), (None, None, None));
    let v: serde_json::Value = serde_json::from_str(&p.to_json()).unwrap();
    assert_eq!(v["clip"], "full");
    assert!(v["clip_span"].is_null());
}

#[test]
fn a_clip_survives_the_plan_round_trip() {
    let mut p = plan();
    p.clip = ClipKind::Drop;
    p.clip_span = Some((101.5, 121.5));
    p.clip_reason = Some("chorus".into());
    p.clip_label = Some("chorus".into());
    let j = p.to_json();
    let v: serde_json::Value = serde_json::from_str(&j).unwrap();
    assert_eq!(v["clip"], "drop");
    assert_eq!(v["clip_span"], serde_json::json!([101.5, 121.5]));
    assert_eq!(RenderPlan::from_json(&j).unwrap(), p);
}

#[test]
fn a_programmatic_caller_cannot_bypass_the_clip_seconds_check() {
    let dir = Tmp::new("clip-seconds");
    let src = flat_wav(&dir, PIPELINE_RATE as usize);
    for v in [0.0, -2.0, f64::NAN] {
        let mut args = projectmp4::render::plan::RenderArgs::new("flat.wav");
        args.clip = ClipKind::Drop;
        args.clip_seconds = Some(v);
        let e = projectmp4::render::plan::render_plan_from_source(&args, src.clone()).unwrap_err().to_string();
        assert!(e.contains("--clip-seconds") && e.contains(&format!("{v}")), "{e}");
    }
}

#[test]
fn trim_of_a_handful_of_frames_is_never_silent() {
    let dir = Tmp::new("clip-trim-tiny");
    let src = flat_wav(&dir, 100);
    for n in 1..=4usize {
        let s = read_f32(&trim(&src, 10.0 / PIPELINE_RATE as f64, (10 + n) as f64 / PIPELINE_RATE as f64, 1.5, &dir).unwrap().wav);
        assert_eq!(s.len(), 2 * n);
        assert!(s.iter().any(|v| *v > 0.5), "{n}-frame clip is silent: {s:?}");
    }
}

#[test]
fn an_impossible_target_is_refused_before_any_stem_work() {
    // 141 s: long enough that `want` (200) clamps to 141, which X still refuses.
    let dir = Tmp::new("clip-target");
    let src = flat_wav(&dir, 141 * PIPELINE_RATE as usize);
    let mut args = projectmp4::render::plan::RenderArgs::new("flat.wav");
    args.clip = ClipKind::Drop;
    args.target = Target::X;
    args.clip_seconds = Some(200.0);
    let e = projectmp4::render::plan::render_plan_from_source(&args, src).unwrap_err().to_string();
    assert!(e.contains("--target x allows 140 s") && e.contains("141.0 s"), "{e}");
}

#[test]
fn asking_for_more_than_the_track_holds_is_a_legal_request() {
    use projectmp4::render::plan::check_clip_target;
    // Regression: both of these were refused when the check used `want` unclamped.
    check_clip_target(Target::X, 180.0, 90.0).expect("90 s track, x: clamps to the whole track");
    check_clip_target(Target::Discord, 1000.0, 60.0).expect("60 s track, discord: clamps to the whole track");
    // ...and a request the target cannot accept is still refused.
    assert!(check_clip_target(Target::X, 180.0, 300.0).is_err());
}

#[test]
fn a_bad_fps_and_flash_rate_are_refused_on_the_source_path_too() {
    let dir = Tmp::new("clip-args");
    let src = flat_wav(&dir, PIPELINE_RATE as usize);
    let mut args = projectmp4::render::plan::RenderArgs::new("flat.wav");
    args.fps = 5;
    let e = projectmp4::render::plan::render_plan_from_source(&args, src.clone()).unwrap_err().to_string();
    assert!(e.contains("--fps 5"), "{e}");
    let mut args = projectmp4::render::plan::RenderArgs::new("flat.wav");
    args.flash_rate = 2.0;
    let e = projectmp4::render::plan::render_plan_from_source(&args, src).unwrap_err().to_string();
    assert!(e.contains("--flash-rate"), "{e}");
}

#[test]
fn clip_seconds_with_the_full_clip_is_refused() {
    let dir = Tmp::new("clip-full-seconds");
    let src = flat_wav(&dir, PIPELINE_RATE as usize);
    let mut args = projectmp4::render::plan::RenderArgs::new("flat.wav");
    args.clip_seconds = Some(30.0);
    let e = projectmp4::render::plan::render_plan_from_source(&args, src).unwrap_err().to_string();
    assert!(e.contains("--clip-seconds 30") && e.contains("--clip full"), "{e}");
}

#[test]
fn a_bad_limit_is_refused_on_the_source_path_too() {
    let dir = Tmp::new("clip-limit");
    let src = flat_wav(&dir, PIPELINE_RATE as usize);
    let mut args = projectmp4::render::plan::RenderArgs::new("flat.wav");
    args.limit_seconds = Some(-1.0);
    let e = projectmp4::render::plan::render_plan_from_source(&args, src).unwrap_err().to_string();
    assert!(e.contains("--limit-seconds") && e.contains("-1"), "{e}");
}

#[test]
fn choose_refuses_non_finite_confidence_and_energy() {
    let s = [sec(0.0, 10.0, CHORUS, f32::NAN)];
    assert!(choose(&s, &[], &[0.1], None, 90.0, ClipKind::Drop).unwrap_err().to_string().contains("NaN"));
    let s = [sec(0.0, 10.0, CHORUS, 0.5)];
    assert!(choose(&s, &[], &[f32::INFINITY], None, 90.0, ClipKind::Drop).unwrap_err().to_string().contains("inf"));
}
