//! The load-bearing spike: drive libprojectM with no window, into an FBO we
//! own, and read the pixels back.
//!
//! Note for anyone adding tests here: **projectM renders are not reproducible
//! run to run, even with the frame time pinned.** `PresetState`'s constructor
//! seeds `hueRandomOffsets` from `std::random_device`, and `MilkdropNoise`
//! seeds its noise textures from `system_clock::now()`; neither is reachable
//! from the C API. Two renders of the *same* preset were measured to differ in
//! 4-50% of pixels. Assert on structural statistics, never on pixel equality
//! or on a pixel-difference threshold between two renders.

use std::path::Path;

use projectmp4::ffi;
use projectmp4::gl_context::GlContext;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const SAMPLE_RATE: f64 = 48_000.0;
const FPS: f64 = 60.0;

/// One frame's worth of a loud 60 Hz stereo sine burst at 48 kHz, interleaved
/// LRLR. 60 Hz sits in projectM's bass band, so `bass_att` climbs over the
/// first frames and the fixture's per-frame equations have something to chew on.
fn sine_burst_48k() -> Vec<f32> {
    let per_channel = (SAMPLE_RATE / FPS) as usize; // 800
    let mut out = Vec::with_capacity(per_channel * 2);
    for i in 0..per_channel {
        let t = i as f64 / SAMPLE_RATE;
        let s = (std::f64::consts::TAU * 60.0 * t).sin() as f32 * 0.9;
        out.push(s);
        out.push(s);
    }
    out
}

fn fixture() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/basic.milk"
    ))
}

/// Renders 30 frames of the sine burst and reads the colour attachment back.
/// `preset = None` leaves projectM on its built-in idle preset.
fn render_30_frames(preset: Option<&Path>) -> Vec<u8> {
    // `pm` borrows `gl`, so the compiler, not the ordering of these two lines,
    // is what keeps the context alive through `projectm_destroy`.
    let gl = GlContext::new(WIDTH, HEIGHT).expect("headless context");
    let pm = gl.create_instance().expect("projectM instance");
    pm.set_window_size(WIDTH, HEIGHT);
    pm.set_mesh_size(48, 32);
    pm.set_fps(FPS as i32);
    if let Some(path) = preset {
        // Errors are the business of the tests below; a failed load here just
        // leaves the idle preset up, which is what they measure.
        let _ = pm.load_preset_file(path, false);
    }
    for n in 0..30 {
        // presets need a few frames to develop
        pm.pcm_add_float(&sine_burst_48k(), ffi::Channels::Stereo);
        pm.set_frame_time(n as f64 / FPS);
        pm.render_frame_fbo(gl.fbo());
    }
    gl.read_rgba().expect("read pixels")
}

#[test]
fn renders_a_non_black_frame_offscreen() {
    let px = render_30_frames(Some(fixture()));
    assert_eq!(px.len(), WIDTH as usize * HEIGHT as usize * 4);
    assert!(
        px.chunks(4).any(|p| p[0] > 8 || p[1] > 8 || p[2] > 8),
        "frame was all black"
    );
}

/// Fraction of pixels with any channel above the non-black threshold. This is
/// a *structural* statistic, not a pixel comparison: see the module note on
/// why two renders of the same preset are never byte-identical.
fn lit_fraction(px: &[u8]) -> f64 {
    let lit = px
        .chunks(4)
        .filter(|p| p[0] > 8 || p[1] > 8 || p[2] > 8)
        .count();
    lit as f64 / (px.len() / 4) as f64
}

/// `projectm_load_preset_file` returns `void` and only logs: on failure the
/// bright idle "M" preset silently stays up and would satisfy the non-black
/// assertion above. Two independent signals pin the fixture down -- the
/// synchronous preset-switch-failed callback, and the fact that the fixture
/// renders mostly black (~28% lit) where idle renders nearly full (~99.6%).
#[test]
fn fixture_preset_displaces_the_idle_preset() {
    let gl = GlContext::new(WIDTH, HEIGHT).expect("headless context");
    let pm = gl.create_instance().expect("projectM instance");
    pm.set_window_size(WIDTH, HEIGHT);
    pm.load_preset_file(fixture(), false)
        .expect("tests/fixtures/basic.milk must load");

    let lit = lit_fraction(&render_30_frames(Some(fixture())));
    assert!(
        lit < 0.70,
        "fixture render lit {:.1}% of pixels, which is idle-preset territory -- \
         basic.milk probably did not take effect",
        lit * 100.0
    );
}

/// Negative control for the test above: both of its signals must actually
/// move when the preset does not load. Without this, a guard that nothing
/// could ever trip would still read as passing.
#[test]
fn a_failed_preset_load_is_indistinguishable_from_idle() {
    let missing = Path::new("/nonexistent/projectmp4-no-such-preset.milk");

    let gl = GlContext::new(WIDTH, HEIGHT).expect("headless context");
    let pm = gl.create_instance().expect("projectM instance");
    pm.set_window_size(WIDTH, HEIGHT);
    assert!(
        pm.load_preset_file(missing, false).is_err(),
        "a nonexistent preset path reported success"
    );

    let lit = lit_fraction(&render_30_frames(Some(missing)));
    assert!(
        lit > 0.90,
        "expected the idle preset to be left on screen after a failed load, \
         but only {:.1}% of pixels were lit",
        lit * 100.0
    );
}
