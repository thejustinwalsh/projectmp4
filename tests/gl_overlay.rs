//! The preview's GPU overlay against the render's CPU composite. Geometry
//! cannot diverge (anchors and the tile index are shared with `Overlay::draw`),
//! so this pins what is GPU-only: the shader's orientation and the blend.

use projectmp4::gl_context::GlContext;
use projectmp4::gl_overlay::GlOverlay;
use projectmp4::render::{MarkStyle, Overlay};
use std::ffi::c_void;
#[path = "common/tmp.rs"]
mod tmp;
use tmp::Tmp;

const W: u32 = 1920;
const H: u32 = 1080;
const CELL: u32 = 30;

#[link(name = "OpenGL", kind = "framework")]
unsafe extern "C" {
    fn glBindFramebuffer(target: u32, fb: u32);
    fn glEnable(cap: u32);
    fn glDisable(cap: u32);
    fn glIsEnabled(cap: u32) -> u8;
    fn glScissor(x: i32, y: i32, w: i32, h: i32);
    fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    fn glClear(mask: u32);
    fn glGetIntegerv(p: u32, out: *mut i32);
    fn glBlendFunc(s: u32, d: u32);
    fn glActiveTexture(t: u32);
    fn glBindTexture(target: u32, id: u32);
    fn glGenTextures(n: i32, ids: *mut u32);
    fn glDeleteTextures(n: i32, ids: *const c_void);
    fn glIsProgram(id: u32) -> u8;
    fn glIsTexture(id: u32) -> u8;
    fn glIsVertexArray(id: u32) -> u8;
}

const FRAMEBUFFER: u32 = 0x8D40;
const SCISSOR_TEST: u32 = 0x0C11;
const COLOR_BUFFER_BIT: u32 = 0x4000;
const BLEND: u32 = 0x0BE2;
const TEXTURE_2D: u32 = 0x0DE1;
const TEXTURE0: u32 = 0x84C0;
const TEXTURE1: u32 = 0x84C1;

fn geti(p: u32) -> i32 {
    let mut v = 0;
    unsafe { glGetIntegerv(p, &mut v) };
    v
}

/// Colour of background cell (`cx`, `cy`), `cy` counted from the top. Varies in
/// both directions and in every channel, so a blend against it is not a blend against black.
fn cell(cx: u32, cy: u32) -> [u8; 3] {
    [(cx * 4 + 3) as u8, (cy * 7 + 20) as u8, ((cx * 5 + cy * 3) % 200 + 30) as u8]
}

fn cpu_background() -> Vec<u8> {
    let mut f = vec![255u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let c = cell(x / CELL, y / CELL);
            f[((y * W + x) * 4) as usize..][..3].copy_from_slice(&c);
        }
    }
    f
}

/// The same picture drawn into the bound framebuffer, cell by cell (GL rows run bottom-up).
fn gpu_background() {
    unsafe {
        glEnable(SCISSOR_TEST);
        for cy in 0..H / CELL {
            for cx in 0..W / CELL {
                let c = cell(cx, cy);
                glScissor((cx * CELL) as i32, (H - (cy + 1) * CELL) as i32, CELL as i32, CELL as i32);
                glClearColor(c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0);
                glClear(COLOR_BUFFER_BIT);
            }
        }
        glDisable(SCISSOR_TEST);
    }
}

fn flip_rows(px: &[u8]) -> Vec<u8> {
    let row = (W * 4) as usize;
    px.chunks_exact(row).rev().flatten().copied().collect()
}

#[test]
fn the_gpu_overlay_matches_the_cpu_composite_over_a_gradient() {
    // Cover art: opaque, so it exercises the non-text path.
    let dir = Tmp::new("gl-overlay");
    let art = dir.join("art.png");
    let img = image::RgbaImage::from_fn(180, 180, |x, y| image::Rgba([(x * 255 / 179) as u8, (y * 255 / 179) as u8, 90, 255]));
    img.save(&art).unwrap();

    let overlay = Overlay::new("Oracle Title", Some("An Artist"), Some(&art), MarkStyle::Word, true).unwrap();
    let gl = GlContext::new(W, H).unwrap();
    let mut gpu_overlay = GlOverlay::new(&overlay, W, H).unwrap();

    for bass in [0.0f32, 0.7] {
        // GPU.
        unsafe { glBindFramebuffer(FRAMEBUFFER, gl.fbo()) };
        gpu_background();
        // Sentinel state the draw must hand back.
        let mut sentinel_tex = 0u32;
        unsafe {
            glActiveTexture(TEXTURE0);
            glGenTextures(1, &mut sentinel_tex);
            glBindTexture(TEXTURE_2D, sentinel_tex);
            glActiveTexture(TEXTURE1);
            glDisable(BLEND);
            glBlendFunc(0x0302, 1); // SRC_ALPHA, ONE
        }
        gpu_overlay.draw(&overlay, W, H, bass);
        unsafe {
            assert_eq!(geti(0x84E0), TEXTURE1 as i32, "active texture unit restored");
            assert_eq!(glIsEnabled(BLEND), 0, "blend enable restored");
            assert_eq!((geti(0x80C9), geti(0x80C8), geti(0x80CB), geti(0x80CA)), (0x0302, 1, 0x0302, 1), "blend function restored");
            assert_eq!(geti(0x8B8D), 0, "program restored");
            assert_eq!(geti(0x85B5), 0, "vertex array restored");
            glActiveTexture(TEXTURE0);
            assert_eq!(geti(0x8069), sentinel_tex as i32, "texture binding on unit 0 restored");
            glBindTexture(TEXTURE_2D, 0);
            glDeleteTextures(1, &sentinel_tex as *const u32 as *const c_void);
        }
        let gpu = flip_rows(&gl.read_rgba().unwrap());

        // CPU.
        let bg = cpu_background();
        let mut cpu = bg.clone();
        overlay.draw(&mut cpu, W, H, bass);

        // The background itself must have come out identical, or the comparison means nothing.
        // (Checked on a pixel the overlay does not touch.)
        let probe = ((540 * W + 960) * 4) as usize;
        assert_eq!(&gpu[probe..probe + 3], &bg[probe..probe + 3], "gradient background differs");

        let mut footprint_mismatch = 0usize;
        let mut covered = 0usize;
        let mut max_diff = 0i32;
        let mut sum = 0i64;
        let mut n = 0i64;
        let layers = [overlay.block_layer(W, H).unwrap(), overlay.mark_layer(overlay.mark_index(bass), W, H)];
        let in_rect = |x: u32, y: u32| layers.iter().any(|l| (x as i64) >= l.x && (x as i64) < l.x + l.w as i64 && (y as i64) >= l.y && (y as i64) < l.y + l.h as i64);
        for y in 0..H {
            for x in 0..W {
                let i = ((y * W + x) * 4) as usize;
                let cpu_hit = cpu[i..i + 3] != bg[i..i + 3];
                let gpu_hit = gpu[i..i + 3] != bg[i..i + 3];
                covered += cpu_hit as usize;
                footprint_mismatch += (cpu_hit != gpu_hit) as usize;
                if in_rect(x, y) {
                    for c in 0..3 {
                        let d = (cpu[i + c] as i32 - gpu[i + c] as i32).abs();
                        max_diff = max_diff.max(d);
                        sum += d as i64;
                        n += 1;
                    }
                } else {
                    assert!(cpu[i..i + 3] == gpu[i..i + 3], "pixel ({x},{y}) outside the overlay rectangles differs");
                }
            }
        }
        let mean = sum as f64 / n as f64;
        eprintln!("bass {bass}: covered {covered} px, footprint mismatches {footprint_mismatch}, max diff {max_diff}, mean diff over rectangles {mean:.5}");
        assert!(covered > 5000, "the overlay drew almost nothing ({covered} px)");
        assert_eq!(footprint_mismatch, 0, "bass {bass}: alpha footprint");
        assert!(max_diff <= 2, "bass {bass}: max per-channel difference {max_diff}");
        assert!(mean < 0.05, "bass {bass}: mean difference {mean}");
    }
    gl.make_current().unwrap();
    gpu_overlay.delete();
}

/// `read_rgba_into` fills a caller's buffer, overwriting every byte of it, and refuses a
/// buffer of the wrong size rather than reading into part of it.
#[test]
fn read_rgba_into_overwrites_a_reused_buffer_and_checks_its_length() {
    let gl = GlContext::new(64, 48).unwrap();
    unsafe {
        glBindFramebuffer(FRAMEBUFFER, gl.fbo());
        glClearColor(0.2, 0.4, 0.6, 1.0);
        glClear(COLOR_BUFFER_BIT);
    }
    let want = gl.read_rgba().unwrap();
    assert_eq!(want.len(), 64 * 48 * 4);
    assert_eq!(&want[..4], &[51, 102, 153, 255]);
    let mut buf = vec![0xABu8; want.len()];
    gl.read_rgba_into(&mut buf).unwrap();
    assert_eq!(buf, want, "stale bytes survived the read");
    assert!(gl.read_rgba_into(&mut vec![0u8; want.len() - 1]).is_err());
    assert!(gl.read_rgba_into(&mut vec![0u8; want.len() + 1]).is_err());
}

/// Names of the program, texture and vertex-array objects alive in the current context.
fn live_objects() -> Vec<(&'static str, u32)> {
    let mut v = Vec::new();
    for id in 1..512u32 {
        unsafe {
            if glIsProgram(id) != 0 {
                v.push(("program", id));
            }
            if glIsTexture(id) != 0 {
                v.push(("texture", id));
            }
            if glIsVertexArray(id) != 0 {
                v.push(("vertex array", id));
            }
        }
    }
    v
}

/// Dropping a `GlOverlay` (an early return, an unwind) frees its GL objects without an
/// explicit `delete()`; and an explicit `delete()` followed by the drop is harmless.
#[test]
fn dropping_a_gl_overlay_frees_its_objects() {
    let gl = GlContext::new(64, 48).unwrap();
    let overlay = Overlay::new("Title", Some("Artist"), None, MarkStyle::Word, true).unwrap();
    let before = live_objects();
    let made = GlOverlay::new(&overlay, 640, 360).unwrap();
    assert!(live_objects().len() > before.len(), "the overlay created nothing; the test proves nothing");
    drop(made);
    assert_eq!(live_objects(), before, "dropping the overlay leaked GL objects");
    let mut twice = GlOverlay::new(&overlay, 640, 360).unwrap();
    twice.delete();
    drop(twice);
    assert_eq!(live_objects(), before);
    drop(gl);
}

#[test]
fn filter_frame_round_trips_the_framebuffer_without_flipping_it() {
    // The preview hands the window's pixels to the flash guard and draws them back.
    // An identity filter must leave the picture exactly as it was, right way up,
    // and an edit must land where it was made.
    let overlay = Overlay::new("T", None, None, MarkStyle::Word, true).unwrap();
    let gl = GlContext::new(W, H).unwrap();
    let mut gpu_overlay = GlOverlay::new(&overlay, W, H).unwrap();
    unsafe { glBindFramebuffer(FRAMEBUFFER, gl.fbo()) };
    gpu_background();
    let before = gl.read_rgba().unwrap();
    let mut buf = Vec::new();
    let mut seen = Vec::new();
    gpu_overlay.filter_frame(&mut buf, W, H, |px| seen = px.to_vec());
    assert_eq!(seen, before, "the filter must see the framebuffer as GL stores it (bottom row first)");
    assert_eq!(gl.read_rgba().unwrap(), before, "an identity filter changed or flipped the picture");

    // Paint the top quarter (the last rows in GL order) white and check where it lands.
    gpu_overlay.filter_frame(&mut buf, W, H, |px| {
        let top = (W * H) as usize * 4 / 4;
        let n = px.len();
        px[n - top..].fill(255);
    });
    let after = gl.read_rgba().unwrap();
    let n = after.len();
    assert!(after[n - 4..n].iter().all(|&b| b == 255), "top-right pixel should be white");
    assert_eq!(after[..4], before[..4], "bottom-left pixel should be untouched");
    gpu_overlay.delete();
}
