use projectmp4::audio::timeline::{Cut, SwitchPoint};
use projectmp4::control::{Action, Applied, RuntimeParams, StemSource};
use projectmp4::preview::{key_action, KeyAction, Load, PreviewState};
use projectmp4::render::plan::RenderPlan;
use projectmp4::render::MarkStyle;
use std::path::PathBuf;
use winit::keyboard::KeyCode;

fn preset(i: usize) -> PathBuf {
    PathBuf::from(format!("pool/preset-{i:02}.milk"))
}

/// A plan the way `render_plan` makes it: segment `i` plays `pool[i % pool.len()]`.
fn plan_of(pool_len: usize, switches: &[(f64, Cut)], fps: u32) -> RenderPlan {
    let pool: Vec<PathBuf> = (0..pool_len).map(preset).collect();
    let switches: Vec<SwitchPoint> = switches.iter().map(|&(t, cut)| SwitchPoint { t, cut }).collect();
    let preset_sequence = (0..=switches.len()).map(|i| pool[i % pool_len].clone()).collect();
    RenderPlan {
        input: PathBuf::from("song.mp3"),
        title: "song".into(),
        artist: None,
        art: None,
        fps,
        duration: 60.0,
        limit_seconds: None,
        total_frames: 60 * fps as u64,
        bpm: 120.0,
        watermark_style: MarkStyle::Word,
        static_overlay: false,
        fast: false,
        target: projectmp4::encode::Target::None,
        bitrate_kbps: None,
        flash_guard: true,
        flash_rate: 0.48,
        packs_revision: None,
        switches,
        preset_sequence,
        pool,
        textures: None,
        clip: projectmp4::audio::sections::clip::ClipKind::Full,
        clip_span: None,
        clip_reason: None,
        clip_label: None,
    }
}

fn state(pool_len: usize, switches: &[(f64, Cut)]) -> PreviewState {
    PreviewState::from_plan(&plan_of(pool_len, switches, 60)).unwrap()
}

#[test]
fn every_key_still_maps_to_the_same_verb() {
    let mut s = state(40, &[(8.0, Cut::Soft)]);
    assert_eq!(key_action(KeyCode::Escape, &mut s), Action::Quit);
    assert_eq!(key_action(KeyCode::ArrowRight, &mut s), Action::Load(Load { path: preset(1), smooth: true }));
    assert_eq!(key_action(KeyCode::ArrowLeft, &mut s), KeyAction::Load(Load { path: preset(0), smooth: true }));
    assert_eq!(key_action(KeyCode::KeyL, &mut s), Action::Lock(true));
    assert_eq!(key_action(KeyCode::KeyP, &mut s), Action::Print(preset(0)));
    assert_eq!(key_action(KeyCode::KeyZ, &mut s), Action::Ignored);
}

#[test]
fn continuous_verbs_clamp_to_their_range() {
    assert_eq!(Action::drive(9.0), Action::Drive(2.0));
    assert_eq!(Action::drive(-1.0), Action::Drive(0.0));
    assert_eq!(Action::drive(f32::NAN), Action::Drive(1.0));
    assert_eq!(Action::beat_sensitivity(9.0), Action::BeatSensitivity(5.0));
    assert_eq!(Action::beat_sensitivity(-1.0), Action::BeatSensitivity(0.0));
    assert_eq!(Action::beat_sensitivity(f32::NAN), Action::BeatSensitivity(1.0));
    assert_eq!(Action::stem_blend(1.4), Action::StemBlend(1.0));
    assert_eq!(Action::stem_blend(-0.2), Action::StemBlend(0.0));
    assert_eq!(Action::stem_blend(f32::NAN), Action::StemBlend(0.5));
}

#[test]
fn runtime_params_start_neutral() {
    let p = RuntimeParams::new(true, false);
    assert_eq!(p.drive, 1.0);
    assert_eq!(p.stem_blend, 0.5);
    assert_eq!(p.beat_sensitivity, 1.0);
    assert!(!p.paused);
    assert!(p.flash_guard);
    assert!(!p.smooth_cuts);
}

#[test]
fn apply_moves_only_the_param_its_verb_names() {
    let mut p = RuntimeParams::new(true, true);
    let before = p.clone();
    assert_eq!(p.apply(&Action::drive(1.5)), Some(Applied::Changed));
    assert_eq!(p.drive, 1.5);
    assert_eq!(RuntimeParams { drive: before.drive, ..p.clone() }, before);
    // A hand-built out-of-range verb is still clamped on the way in.
    p.apply(&Action::Drive(50.0));
    assert_eq!(p.drive, 2.0);
}

#[test]
fn apply_declines_the_verbs_the_preview_owns() {
    let mut p = RuntimeParams::new(true, true);
    let owned_elsewhere = [
        Action::Load(Load { path: preset(0), smooth: true }),
        Action::Lock(true),
        Action::StepPreset { forward: true },
        Action::JumpPreset { slot: 3 },
        Action::Resync,
        Action::Print(preset(0)),
        Action::Quit,
    ];
    for a in &owned_elsewhere {
        assert_eq!(p.apply(a), None, "{a:?}");
    }
    assert_eq!(p, RuntimeParams::new(true, true));
}

#[test]
fn a_verb_that_changes_nothing_reports_no_change() {
    let mut p = RuntimeParams::new(true, true);
    assert_eq!(p.apply(&Action::FlashGuard(true)), Some(Applied::NoChange));
    assert_eq!(p.apply(&Action::FlashGuard(false)), Some(Applied::Changed));
    assert_eq!(p.apply(&Action::StemSnap(StemSource::All)), Some(Applied::NoChange));
    assert_eq!(p.apply(&Action::TransportToggle), Some(Applied::Changed));
    assert!(p.paused);
}
