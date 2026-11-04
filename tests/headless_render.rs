//! The load-bearing spike: drive libprojectM with no window, into an FBO we
//! own, and read the pixels back.

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
    // Declared first so it is dropped last: `projectm_destroy` needs the
    // context still current.
    let gl = GlContext::new(WIDTH, HEIGHT).expect("headless context");
    let pm = ffi::Instance::new(gl.loader()).unwrap();
    pm.set_window_size(WIDTH, HEIGHT);
    pm.set_mesh_size(48, 32);
    pm.set_fps(FPS as i32);
    if let Some(path) = preset {
        pm.load_preset_file(path, false).expect("preset path");
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

/// `projectm_load_preset_file` reports failure only through projectM's log: a
/// bad preset silently leaves the idle preset on screen, which is itself bright
/// and would satisfy the non-black assertion above. Comparing the two renders
/// makes the fixture's validity load-bearing on a test rather than on eyeballs.
#[test]
fn fixture_preset_displaces_the_idle_preset() {
    let mine = render_30_frames(Some(fixture()));
    let idle = render_30_frames(None);
    assert_eq!(mine.len(), idle.len());

    let differing = mine
        .chunks(4)
        .zip(idle.chunks(4))
        .filter(|(a, b)| {
            a[0].abs_diff(b[0]) > 16 || a[1].abs_diff(b[1]) > 16 || a[2].abs_diff(b[2]) > 16
        })
        .count();
    let total = mine.len() / 4;
    assert!(
        differing * 10 > total,
        "tests/fixtures/basic.milk rendered almost identically to the idle preset \
         ({differing}/{total} pixels differ) -- it probably failed to load"
    );
}
