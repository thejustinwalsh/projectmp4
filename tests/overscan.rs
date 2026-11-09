//! The overscan geometry, and the blit the preview uses to show the frame the
//! render keeps.
//!
//! The bug these pin: `render` drew 5% wider and taller and cropped the centre,
//! while `preview` drew at window size straight to the window. The preview
//! therefore showed the MilkDrop edge artifacts the render throws away, against
//! a CLI that documents "the preview of a track is the render of it".
//!
//! The preview itself opens a window and is not run here. What is checked is
//! everything that decides the framing: the shared geometry, and the blit
//! executed on a real headless GL context against a known pattern.

use projectmp4::gl_context::{GlContext, Offscreen};
use projectmp4::render::plan::{overscan_margin, overscanned, HEIGHT, RENDER_HEIGHT, RENDER_WIDTH, WIDTH};

#[test]
fn the_margin_is_whole_pixels_on_both_sides_at_every_size() {
    // Why this is a function of the size rather than a ratio applied twice: a
    // flat 21/20 of 540 is 567, an odd number, so the margin would be 13.5 rows
    // and the preview's framing would sit half a row off the render's.
    for (w, h) in [(1920, 1080), (960, 540), (1280, 720), (1366, 768), (2560, 1440), (8, 8), (1, 1)] {
        let (ow, oh) = overscanned(w, h);
        let (mx, my) = overscan_margin(w, h);
        assert_eq!((ow - w) % 2, 0, "{w}x{h}: {ow} leaves an odd horizontal margin");
        assert_eq!((oh - h) % 2, 0, "{w}x{h}: {oh} leaves an odd vertical margin");
        assert_eq!(((ow - w) / 2, (oh - h) / 2), (mx, my), "{w}x{h}");
        assert!(ow >= w && oh >= h, "{w}x{h}: overscan must not shrink the frame");
    }
}

#[test]
fn the_render_constants_are_what_the_shared_geometry_produces() {
    // The figures the render has always used, now derived rather than written
    // down twice: 21/20 of 1920x1080 is 2016x1134, margin 48 and 27.
    assert_eq!(overscanned(WIDTH, HEIGHT), (RENDER_WIDTH, RENDER_HEIGHT));
    assert_eq!((RENDER_WIDTH, RENDER_HEIGHT), (2016, 1134));
    assert_eq!(overscan_margin(WIDTH, HEIGHT), (48, 27));
    // And the margin the render's `crop_flip` takes off each side is the same
    // number, which is the invariant the preview now shares.
    assert_eq!(overscan_margin(WIDTH, HEIGHT), ((RENDER_WIDTH - WIDTH) / 2, (RENDER_HEIGHT - HEIGHT) / 2));
}

/// The blit keeps the centre and nothing but the centre.
///
/// The pattern makes an off-by-one impossible to pass: the margin is red and
/// the kept rectangle is green, so a blit shifted by a single pixel in any
/// direction drags a row or column of red into the result.
#[test]
fn the_blit_drops_exactly_the_overscan_margin() {
    const W: u32 = 160;
    const H: u32 = 90;
    let (ow, oh) = overscanned(W, H);
    let (mx, my) = overscan_margin(W, H);
    assert!(mx > 0 && my > 0, "the test needs a real margin, got {mx}x{my}");

    // The context only has to exist and be current; its own size is irrelevant.
    let _gl = GlContext::new(ow, oh).unwrap();
    let src = Offscreen::new(ow, oh).unwrap();
    let dst = Offscreen::new(W, H).unwrap();

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
    const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
    src.paint_rect(RED, mx, my, W, H, GREEN).unwrap();
    // Blue first, so an all-green result cannot be a blit that did nothing.
    dst.paint_rect(BLUE, 0, 0, 1, 1, BLUE).unwrap();

    let mut before = vec![0u8; (W * H * 4) as usize];
    dst.read_rgba_into(&mut before).unwrap();
    assert_eq!(&before[..4], &[0, 0, 255, 255], "destination did not start blue");

    src.blit_centre(dst.fbo(), W, H).unwrap();

    let mut got = vec![0u8; (W * H * 4) as usize];
    dst.read_rgba_into(&mut got).unwrap();
    let off: Vec<(u32, u32, [u8; 4])> = got
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter(|(_, p)| p[1] < 250 || p[0] > 5 || p[2] > 5)
        .map(|(i, p)| ((i as u32) % W, (i as u32) / W, *p))
        .collect();
    let shown: Vec<_> = off.iter().take(8).collect();
    assert!(off.is_empty(), "{} of {} pixels are not the kept green: {shown:?}", off.len(), W * H);
}

/// A destination the source cannot fill is refused rather than silently
/// letterboxed, and an empty one too.
#[test]
fn a_blit_that_would_not_fit_is_an_error() {
    let _gl = GlContext::new(64, 64).unwrap();
    let src = Offscreen::new(64, 64).unwrap();
    for (w, h) in [(65, 64), (64, 65), (0, 64), (64, 0)] {
        let Err(e) = src.blit_centre(0, w, h) else { panic!("{w}x{h} must be refused") };
        assert!(e.0.contains("cannot yield"), "unhelpful message: {}", e.0);
    }
    // The exact size is allowed, and is a plain copy with no margin.
    assert!(src.blit_centre(src.fbo(), 64, 64).is_ok());
}

#[test]
fn an_empty_offscreen_is_refused() {
    let _gl = GlContext::new(8, 8).unwrap();
    for (w, h) in [(0, 8), (8, 0), (0, 0)] {
        assert!(Offscreen::new(w, h).is_err(), "{w}x{h}");
    }
}
