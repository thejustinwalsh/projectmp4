//! The testable core of the realtime preview: which preset is showing, which
//! frame is due for the audio position, and how the audio callback fills its
//! buffer. The window, the GL context and the output device are not tested.

use projectmp4::audio::timeline::{Cut, SwitchPoint};
use projectmp4::audio::DriverKind;
use projectmp4::preview::{fill_output, heard_secs, key_action, KeyAction, Pacer, PreviewState};
use winit::keyboard::KeyCode;
use projectmp4::render::plan::RenderPlan;
use projectmp4::render::MarkStyle;
use std::path::PathBuf;

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
        driver: DriverKind::Mix,
        watermark_style: MarkStyle::Word,
        static_overlay: false,
        fast: false,
        flash_guard: true,
        flash_limit: 0.010,
        packs_revision: None,
        switches,
        preset_sequence,
        pool,
        textures: None,
    }
}

fn state(pool_len: usize, switches: &[(f64, Cut)]) -> PreviewState {
    PreviewState::from_plan(&plan_of(pool_len, switches, 60)).unwrap()
}

#[test]
fn preview_state_advances_presets_and_reports_the_current_name() {
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Hard)]);
    let first = s.current().to_owned();
    assert_eq!(first, preset(0), "opens on the plan's first preset");

    s.next();
    assert_eq!(s.current(), preset(1), "next lands on the pool's next entry");
    s.previous();
    assert_eq!(s.current(), first, "previous returns to the first");
    s.previous();
    assert_eq!(s.current(), preset(39), "previous wraps to the end of the pool");
    s.next();
    assert_eq!(s.current(), first, "next wraps back to the start");

    s.lock();
    let locked = s.current().to_owned();
    s.on_frame(60 * 60);
    assert_eq!(s.current(), locked, "locked preset must not rotate");
}

#[test]
fn stepping_leaves_the_plans_sequence_for_the_pool() {
    // Three segments, forty presets: the arrows must reach presets the plan never uses.
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Soft)]);
    for _ in 0..5 {
        s.next();
    }
    assert_eq!(s.current(), preset(5));
}

#[test]
fn the_schedule_switches_on_the_frame_the_render_would() {
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Hard)]);
    // 8.0 s at 60 fps is frame 480 exactly: the render loads on it, not before.
    assert!(s.on_frame(0).is_none());
    assert!(s.on_frame(479).is_none(), "one frame early");
    let load = s.on_frame(480).expect("switch lands on frame 480");
    assert_eq!(load.path, preset(1));
    assert!(load.smooth, "a soft cut is a smooth transition");
    assert_eq!(s.current(), preset(1));
    assert!(s.on_frame(481).is_none(), "a switch fires once");

    let load = s.on_frame(960).expect("second switch");
    assert_eq!(load.path, preset(2));
    assert!(!load.smooth, "a hard cut is not smooth");
}

#[test]
fn a_stall_takes_the_cut_of_the_switch_now_due_not_the_first_one_passed() {
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Hard)]);
    let load = s.on_frame(1000).unwrap();
    assert_eq!(load.path, preset(2));
    assert!(!load.smooth, "the 16 s switch is hard");
}

#[test]
fn a_stall_across_two_switches_loads_only_the_preset_now_due() {
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Soft)]);
    let load = s.on_frame(1000).expect("both switches are behind us");
    assert_eq!(load.path, preset(2));
    assert!(load.smooth, "the cut that is now due is soft");
    assert_eq!(s.current(), preset(2));
}

#[test]
fn lock_holds_through_switches_and_unlock_rejoins_at_the_next_one() {
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Soft), (24.0, Cut::Soft)]);
    s.next(); // pool[1], off schedule
    s.lock();
    assert!(s.is_locked());
    assert!(s.on_frame(480).is_none(), "locked: the 8 s switch does nothing");
    assert!(s.on_frame(960).is_none(), "locked: the 16 s switch does nothing");
    assert_eq!(s.current(), preset(1));

    s.unlock();
    assert!(!s.is_locked());
    assert_eq!(s.current(), preset(1), "unlocking does not jump");
    let load = s.on_frame(1440).expect("the 24 s switch applies again");
    assert_eq!(load.path, preset(3), "and lands on the plan's preset, not a neighbour of the old one");
}

#[test]
fn off_schedule_is_true_after_a_step_and_cleared_by_the_next_scheduled_switch() {
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Soft)]);
    assert!(!s.off_schedule());
    s.next();
    assert!(s.off_schedule());
    s.on_frame(100);
    assert!(s.off_schedule(), "no switch yet: still off");
    s.on_frame(480);
    assert!(!s.off_schedule(), "the schedule takes over again");
    assert_eq!(s.current(), preset(1));

    // Stepping onto what the schedule would show anyway is not leaving it.
    let mut s = state(40, &[(8.0, Cut::Soft)]);
    s.next();
    s.on_frame(480);
    s.previous();
    s.next();
    assert!(!s.off_schedule());
}

#[test]
fn a_plan_with_no_presets_is_an_error() {
    let mut p = plan_of(3, &[], 60);
    p.pool.clear();
    assert!(PreviewState::from_plan(&p).is_err());
    let mut p = plan_of(3, &[], 60);
    p.preset_sequence.clear();
    assert!(PreviewState::from_plan(&p).is_err());
}

#[test]
fn a_switch_time_that_is_not_a_whole_number_of_frames_lands_on_the_next_frame() {
    // 8.404 s * 60 = 504.24: the render loads on frame 505, the first at or after it.
    // (Rounding would say 504; a value like 507.6 cannot tell round from ceil.)
    let mut s = state(40, &[(8.404, Cut::Soft)]);
    assert!(s.on_frame(504).is_none(), "frame 504 is before the switch");
    assert_eq!(s.on_frame(505).unwrap().path, preset(1));
}

#[test]
fn a_frame_earlier_than_one_already_seen_is_ignored() {
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Soft)]);
    assert_eq!(s.on_frame(500).unwrap().path, preset(1));
    assert!(s.on_frame(100).is_none(), "time does not run backwards");
    assert_eq!(s.current(), preset(1));
    assert!(s.on_frame(501).is_none(), "and the switch is not loaded a second time");
    assert!(s.on_frame(u64::MAX).is_some(), "the largest frame index is an ordinary frame");
    assert_eq!(s.current(), preset(2));
}

// --- Skips: the render's pool cursor -----------------------------------------------

#[test]
fn a_preset_that_will_not_load_moves_every_later_preset_as_it_does_in_the_render() {
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Soft)]);
    assert_eq!(s.current(), preset(0));
    let l = s.load_failed().expect("another preset is available");
    assert_eq!(l.path, preset(1), "the opening preset is replaced by the cursor's next");
    assert_eq!(s.current(), preset(1));
    assert!(!s.off_schedule(), "the replacement is the schedule's own");
    // Without the skip segment 1 would be preset 1. The render's cursor is at 2.
    assert_eq!(s.on_frame(480).unwrap().path, preset(2));
    assert_eq!(s.on_frame(960).unwrap().path, preset(3));
}

#[test]
fn a_failed_scheduled_load_keeps_its_cut_and_a_set_aside_preset_is_never_taken_again() {
    // Pool of 3 walked round: segments 0,1,2,3 would be 0,1,2,0.
    let mut s = state(3, &[(8.0, Cut::Soft), (16.0, Cut::Soft), (24.0, Cut::Soft)]);
    assert_eq!(s.on_frame(480).unwrap().path, preset(1));
    let l = s.load_failed().unwrap();
    assert_eq!((l.path.clone(), l.smooth), (preset(2), true), "retry keeps the soft cut");
    assert_eq!(s.on_frame(960).unwrap().path, preset(0), "cursor moved past 1 and 2");
    assert_eq!(s.on_frame(1440).unwrap().path, preset(2), "the cursor wraps past preset 1, which is set aside for good");
}

#[test]
fn switches_passed_while_locked_still_move_the_cursor() {
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Soft), (24.0, Cut::Soft)]);
    s.lock();
    s.on_frame(480);
    s.on_frame(960);
    s.unlock();
    assert_eq!(s.on_frame(1440).unwrap().path, preset(3));
}

#[test]
fn a_failed_manual_step_goes_on_in_the_same_direction_and_the_arrows_then_skip_it() {
    let mut s = state(40, &[]);
    s.next();
    assert_eq!(s.current(), preset(1));
    assert!(s.off_schedule());
    assert_eq!(s.load_failed().unwrap().path, preset(2));
    assert_eq!(s.current(), preset(2));
    s.previous();
    assert_eq!(s.current(), preset(0), "preset 1 is set aside");
    s.next();
    assert_eq!(s.current(), preset(2), "stepping forward from 0 steps over the set-aside 1");
}

#[test]
fn a_failed_backward_step_goes_on_backward() {
    let mut s = state(40, &[]);
    s.previous();
    assert_eq!(s.current(), preset(39));
    assert_eq!(s.load_failed().unwrap().path, preset(38));
}

#[test]
fn when_nothing_will_load_load_failed_says_so() {
    let mut s = state(3, &[]);
    assert!(s.load_failed().is_some());
    assert!(s.load_failed().is_some());
    assert!(s.load_failed().is_none());
}

#[test]
fn a_plan_whose_sequence_is_not_the_pool_walked_in_order_is_refused() {
    let mut p = plan_of(40, &[(8.0, Cut::Soft)], 60);
    p.preset_sequence[1] = preset(7);
    let e = PreviewState::from_plan(&p).unwrap_err();
    assert!(e.0.contains("preset 1"), "{}", e.0);
}

// --- Notices: tell the user when preview can no longer be trusted to match ---------

#[test]
fn an_ordinary_switch_says_nothing() {
    let mut s = state(40, &[(8.0, Cut::Soft)]);
    s.on_frame(480);
    assert!(s.take_notice().is_none());
}

#[test]
fn a_switch_passed_while_locked_is_reported_once() {
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Soft)]);
    s.lock();
    s.on_frame(480);
    let n = s.take_notice().expect("a locked switch is reported");
    assert!(n.contains("locked"), "{n}");
    assert!(s.take_notice().is_none(), "taken once");
    s.on_frame(481);
    assert!(s.take_notice().is_none(), "no switch, no notice");
}

#[test]
fn switches_stalled_over_are_reported_with_their_count() {
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Soft), (24.0, Cut::Soft)]);
    s.on_frame(1500);
    let n = s.take_notice().expect("three switches passed in one frame");
    assert!(n.contains("3 switches"), "{n}");
}

// --- Keys ---------------------------------------------------------------------------

#[test]
fn keys_map_to_actions() {
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Soft)]);
    assert_eq!(key_action(KeyCode::Escape, &mut s), KeyAction::Quit);
    assert_eq!(s.current(), preset(0), "Esc changes nothing");

    match key_action(KeyCode::ArrowRight, &mut s) {
        KeyAction::Load(l) => assert_eq!(l.path, preset(1)),
        other => panic!("{other:?}"),
    }
    assert_eq!(s.current(), preset(1));
    match key_action(KeyCode::ArrowLeft, &mut s) {
        KeyAction::Load(l) => assert_eq!(l.path, preset(0)),
        other => panic!("{other:?}"),
    }
    assert_eq!(s.current(), preset(0));

    assert_eq!(key_action(KeyCode::KeyP, &mut s), KeyAction::Print(preset(0)));
    key_action(KeyCode::ArrowRight, &mut s);
    assert_eq!(key_action(KeyCode::KeyP, &mut s), KeyAction::Print(preset(1)), "P prints what is on screen now");
}

#[test]
fn l_locks_and_a_second_press_unlocks() {
    let mut s = state(40, &[(8.0, Cut::Soft), (16.0, Cut::Soft), (24.0, Cut::Soft)]);
    assert_eq!(key_action(KeyCode::KeyL, &mut s), KeyAction::Lock(true));
    assert!(s.is_locked());
    assert!(s.on_frame(480).is_none(), "locked: the switch is not shown");

    assert_eq!(key_action(KeyCode::KeyL, &mut s), KeyAction::Lock(false));
    assert!(!s.is_locked());
    // Switch 1 passed while locked (cursor 1), so the unlocked 16 s switch takes preset 2.
    assert_eq!(s.on_frame(960).unwrap().path, preset(2), "unlocked: the schedule applies again, from where the cursor is");
}

#[test]
fn an_unmapped_key_does_nothing() {
    let mut s = state(40, &[(8.0, Cut::Soft)]);
    for code in [KeyCode::KeyZ, KeyCode::Space, KeyCode::ArrowUp, KeyCode::Enter] {
        assert_eq!(key_action(code, &mut s), KeyAction::Ignored, "{code:?}");
    }
    assert_eq!(s.current(), preset(0));
    assert!(!s.is_locked());
    assert!(!s.off_schedule());
}

// --- Pacer: pick the frame for the audio position; never replay one -----------

#[test]
fn the_pacer_picks_the_frame_for_the_audio_position_and_counts_what_it_skipped() {
    let mut p = Pacer::new(60);
    let d = p.due(0.0).expect("frame 0 is due at t=0");
    assert_eq!((d.frame, d.dropped), (0, 0));
    assert!(p.due(0.0).is_none(), "the same position is not a new frame");
    assert!(p.due(0.016).is_none(), "still inside frame 0");

    let d = p.due(1.0 / 60.0).expect("frame 1 starts at exactly 1/60 s");
    assert_eq!((d.frame, d.dropped), (1, 0));

    // Rendering fell behind: the audio is at 0.1 s, frame 6. Frames 2..=5 are dropped.
    let d = p.due(0.1).expect("a late frame");
    assert_eq!((d.frame, d.dropped), (6, 4));
    assert_eq!((p.rendered(), p.dropped()), (3, 4));
}

#[test]
fn every_frame_boundary_is_its_own_frame_despite_float_rounding() {
    // 123 / 60 * 60 is 122.99999999999999 in f64; the frame must still be 123.
    for fps in [60u32, 30] {
        let mut p = Pacer::new(fps);
        for k in 0..2000u64 {
            let d = p.due(k as f64 / fps as f64).unwrap_or_else(|| panic!("frame {k} at {fps} fps was not due"));
            assert_eq!((d.frame, d.dropped), (k, 0), "{fps} fps");
        }
    }
}

#[test]
fn a_position_just_before_a_boundary_is_still_the_previous_frame() {
    // One tenth of a millisecond early is 0.6% of a frame at 60 fps: a tolerance
    // wider than float rounding would call it the next frame.
    for fps in [60u32, 30] {
        let mut p = Pacer::new(fps);
        let f = fps as f64;
        assert_eq!(p.due(1.0 / f - 1e-4).unwrap().frame, 0);
        for k in 1..500u64 {
            assert_eq!(p.due(k as f64 / f).unwrap().frame, k, "{fps} fps boundary {k}");
            assert!(p.due((k + 1) as f64 / f - 1e-4).is_none(), "{fps} fps just before {}", k + 1);
        }
    }
}

#[test]
fn a_position_no_track_has_is_ignored_and_the_pacer_carries_on() {
    let mut p = Pacer::new(60);
    assert!(p.due(1e300).is_none(), "frame index saturates at u64::MAX");
    let d = p.due(0.0).expect("frame 0 is still due");
    assert_eq!((d.frame, d.dropped), (0, 0));
}

#[test]
fn the_pacer_never_goes_backwards() {
    let mut p = Pacer::new(60);
    p.due(1.0).unwrap();
    assert!(p.due(0.5).is_none(), "an audio position that moved back draws nothing");
    assert_eq!(p.dropped(), 60, "frames 0..=59 were never drawn");
}

#[test]
fn a_fresh_pacer_ignores_positions_before_the_start() {
    let mut p = Pacer::new(60);
    assert!(p.due(-1.0).is_none());
    assert!(p.due(f64::NAN).is_none());
    // Equivalent mutant for +inf only: without the `is_finite` guard, `inf as u64` saturates to
    // u64::MAX and the later `frame == u64::MAX` check returns None anyway. NaN (above) and
    // -inf (via `secs < 0.0`) are what keep the guard live.
    assert!(p.due(f64::INFINITY).is_none());
    let d = p.due(0.0).expect("frame 0 is still due");
    assert_eq!((d.frame, d.dropped), (0, 0));
}

#[test]
fn the_pacer_uses_the_plans_fps() {
    let mut p = Pacer::new(30);
    p.due(0.0).unwrap();
    assert!(p.due(1.0 / 60.0).is_none());
    assert_eq!(p.due(1.0 / 30.0).unwrap().frame, 1);
}

// --- Audio clock ----------------------------------------------------------------

#[test]
fn the_heard_position_trails_what_was_written_by_the_output_latency() {
    // 48000 frames written (1 s). The last one is heard 50 ms after the callback ran.
    assert!((heard_secs(48_000, 48_000, 0.050, 0.0) - 0.95).abs() < 1e-12);
    // 20 ms later we have heard 20 ms more.
    assert!((heard_secs(48_000, 48_000, 0.050, 0.020) - 0.97).abs() < 1e-12);
    // Never ahead of what was written, however stale the callback is.
    assert!((heard_secs(48_000, 48_000, 0.050, 5.0) - 1.0).abs() < 1e-12);
    // Never before the start.
    assert_eq!(heard_secs(100, 48_000, 0.5, 0.0), 0.0);
}

// --- Audio callback -------------------------------------------------------------

fn ramp(frames: usize) -> Vec<f32> {
    (0..frames).flat_map(|i| [i as f32, -(i as f32)]).collect()
}

#[test]
fn fill_output_copies_at_unit_step_and_advances_the_position() {
    let src = ramp(8);
    let mut out = vec![9.0f32; 6]; // 3 stereo frames
    let mut pos = 2.0;
    fill_output(&src, &mut pos, 1.0, &mut out, 2);
    assert_eq!(out, vec![2.0, -2.0, 3.0, -3.0, 4.0, -4.0]);
    assert_eq!(pos, 5.0);
}

#[test]
fn fill_output_plays_silence_past_the_end_and_does_not_stall() {
    let src = ramp(4);
    let mut out = vec![9.0f32; 12]; // 6 frames, only 2 left in the source
    let mut pos = 2.0;
    fill_output(&src, &mut pos, 1.0, &mut out, 2);
    assert_eq!(out, vec![2.0, -2.0, 3.0, -3.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(pos, 8.0, "the position keeps counting so the clock keeps running");
}

#[test]
fn fill_output_resamples_by_interpolating() {
    let src = ramp(8);
    let mut out = vec![0.0f32; 8]; // 4 frames at half speed
    let mut pos = 1.0;
    fill_output(&src, &mut pos, 0.5, &mut out, 2);
    assert_eq!(out, vec![1.0, -1.0, 1.5, -1.5, 2.0, -2.0, 2.5, -2.5]);
    assert_eq!(pos, 3.0);
}

#[test]
fn fill_output_leaves_extra_channels_silent() {
    let src = ramp(4);
    let mut out = vec![9.0f32; 6]; // 2 frames, 3 channels
    let mut pos = 1.0;
    fill_output(&src, &mut pos, 1.0, &mut out, 3);
    assert_eq!(out, vec![1.0, -1.0, 0.0, 2.0, -2.0, 0.0]);
}

#[test]
fn fill_output_serves_a_mono_device_the_left_channel() {
    let src = ramp(4);
    let mut out = vec![9.0f32; 2];
    let mut pos = 1.0;
    fill_output(&src, &mut pos, 1.0, &mut out, 1);
    assert_eq!(out, vec![1.0, 2.0]);
}

