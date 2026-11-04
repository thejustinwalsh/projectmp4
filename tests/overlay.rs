use projectmp4::render::overlay::{ART_PX, SAFE_MARGIN};
use projectmp4::render::{MarkStyle, Overlay};
use std::path::{Path, PathBuf};

const W: u32 = 1920;
const H: u32 = 1080;

#[derive(Clone, Copy)]
enum Region {
    UpperRight,
    LowerLeft,
    Centre,
}

fn black_frame(w: u32, h: u32) -> Vec<u8> {
    [0, 0, 0, 255].repeat((w * h) as usize)
}

fn is_ink(f: &[u8], x: u32, y: u32) -> bool {
    let i = ((y * W + x) * 4) as usize;
    f[i].max(f[i + 1]).max(f[i + 2]) > 24
}

fn bounds(r: Region) -> (u32, u32, u32, u32) {
    match r {
        Region::UpperRight => (W / 2, 0, W, H / 2),
        Region::LowerLeft => (0, H / 2, W / 2, H),
        Region::Centre => (W / 4 + 160, H / 4 + 90, 3 * W / 4 - 160, 3 * H / 4 - 90),
    }
}

/// Count of non-black pixels in the region of a frame that started all black.
fn ink_in_region(f: &[u8], r: Region) -> f64 {
    let (x0, y0, x1, y1) = bounds(r);
    let mut n = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            n += is_ink(f, x, y) as u64;
        }
    }
    n as f64
}

fn bbox_of(r: Region, f: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let (x0, y0, x1, y1) = bounds(r);
    let mut bb: Option<(u32, u32, u32, u32)> = None;
    for y in y0..y1 {
        for x in x0..x1 {
            if is_ink(f, x, y) {
                bb = Some(match bb {
                    None => (x, y, x, y),
                    Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x), d.max(y)),
                });
            }
        }
    }
    bb
}

fn ink_in_border(f: &[u8], m: u32) -> f64 {
    let mut n = 0u64;
    for y in 0..H {
        for x in 0..W {
            if (x < m || y < m || x >= W - m || y >= H - m) && is_ink(f, x, y) {
                n += 1;
            }
        }
    }
    n as f64
}

fn fixture(name: &str, w: u32, h: u32) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("projectmp4-overlay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    // Every channel >= 60 so every art pixel counts as ink.
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(60 + x * 190 / w) as u8, (60 + y * 190 / h) as u8, 220])
    });
    img.save(&path).unwrap();
    path
}
fn art_16x9() -> PathBuf {
    fixture("art16x9.png", 640, 360)
}
fn art_1x1() -> PathBuf {
    fixture("art1x1.png", 360, 360)
}

fn render(art: Option<&Path>, bass: f32) -> Vec<u8> {
    let mut f = black_frame(W, H);
    Overlay::new("Hollow Signal", Some("GH0STF1R3"), art, MarkStyle::Word, true)
        .unwrap()
        .draw(&mut f, W, H, bass);
    f
}
fn drawn_with(art: PathBuf) -> Vec<u8> {
    render(Some(&art), 0.0)
}
fn drawn_at_bass(b: f32) -> Vec<u8> {
    render(Some(&art_1x1()), b)
}

#[test]
fn draws_into_the_two_expected_corners() {
    let art = art_1x1();
    let both = ink_in_region(&render(Some(&art), 0.0), Region::UpperRight);
    let text_only = ink_in_region(&render(None, 0.0), Region::UpperRight);
    let a2 = (ART_PX * ART_PX) as f64;

    // Text alone: a two-line block of real glyphs, not a stray pixel.
    assert!((1500.0..30000.0).contains(&text_only), "text-only ink {text_only}");
    // Art adds exactly its square (within crop/rounding), and the text is still there.
    // If only the art drew: both < text_only + a2. If only the text drew: both == text_only.
    assert!(both >= text_only + 0.98 * a2 && both <= text_only + a2 + 1.0, "both {both}, text {text_only}, art {a2}");

    let f = render(Some(&art), 0.0);
    let mark = ink_in_region(&f, Region::LowerLeft);
    assert!((600.0..5000.0).contains(&mark), "watermark ink {mark}");
    assert_eq!(ink_in_region(&f, Region::Centre), 0.0, "overlay must not cover the visuals");
}

#[test]
fn missing_art_still_renders_the_text() {
    let mut f = black_frame(W, H);
    Overlay::new("T", None, None, MarkStyle::Word, true).unwrap().draw(&mut f, W, H, 0.0);
    // One glyph at 34px bold: well above a stray pixel, well below a block.
    let ink = ink_in_region(&f, Region::UpperRight);
    assert!((100.0..3000.0).contains(&ink), "ink {ink}");
    let bb = bbox_of(Region::UpperRight, &f).unwrap();
    assert!(bb.2 < W - SAFE_MARGIN && bb.1 >= SAFE_MARGIN, "{bb:?}");
}

#[test]
fn unreadable_art_is_skipped_not_fatal() {
    let bad = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    let mut f = black_frame(W, H);
    Overlay::new("Hollow Signal", Some("GH0STF1R3"), Some(bad), MarkStyle::Word, true)
        .unwrap()
        .draw(&mut f, W, H, 0.0);
    let skipped = ink_in_region(&f, Region::UpperRight);
    let with_art = ink_in_region(&render(Some(&art_1x1()), 0.0), Region::UpperRight);
    let text_only = ink_in_region(&render(None, 0.0), Region::UpperRight);
    assert!(skipped > 1500.0, "text must still draw, got {skipped}");
    assert_eq!(skipped, text_only, "skipped art must leave exactly the text-only overlay");
    assert!(with_art - skipped > 0.98 * (ART_PX * ART_PX) as f64, "valid art must add ink: {with_art} vs {skipped}");
    assert!(ink_in_region(&f, Region::LowerLeft) > 600.0, "watermark must still draw");
}

#[test]
fn non_square_art_is_cropped_to_square_without_shifting_layout() {
    let a = bbox_of(Region::UpperRight, &drawn_with(art_16x9()));
    let b = bbox_of(Region::UpperRight, &drawn_with(art_1x1()));
    let a = a.expect("16:9 render drew nothing");
    assert!(a.2 - a.0 > ART_PX && a.3 - a.1 >= ART_PX - 1, "box too small to contain the art: {a:?}");
    assert_eq!(Some(a), b);
}

#[test]
fn everything_stays_inside_the_64px_safe_margin() {
    // Quiet and full bass: the loud case is where growth could cross the margin.
    for bass in [0.0, 0.5, 1.0] {
        let f = drawn_at_bass(bass);
        assert!(ink_in_region(&f, Region::LowerLeft) > 0.0 && ink_in_region(&f, Region::UpperRight) > 0.0);
        assert_eq!(ink_in_border(&f, SAFE_MARGIN), 0.0, "bass {bass}");
    }
}

#[test]
fn bass_scales_the_watermark_within_bounds() {
    let quiet = ink_in_region(&drawn_at_bass(0.0), Region::LowerLeft);
    let loud = ink_in_region(&drawn_at_bass(1.0), Region::LowerLeft);
    assert!(loud > quiet && loud < quiet * 1.6, "quiet {quiet}, loud {loud}");
}

#[test]
fn halo_darkens_a_bright_background_around_the_mark() {
    let mut f = [255u8, 255, 255, 255].repeat((W * H) as usize);
    Overlay::new("Hollow Signal", Some("GH0STF1R3"), None, MarkStyle::Word, true)
        .unwrap()
        .draw(&mut f, W, H, 0.0);
    // Dark pixels on a white field exist only where the halo/scrim lies.
    let (x0, y0, x1, y1) = bounds(Region::LowerLeft);
    let mut dark = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * W + x) * 4) as usize;
            dark += (f[i] < 200) as u64;
        }
    }
    assert!(dark > 1500, "halo missing: {dark} darkened pixels");
    // And the border stays pristine white.
    for y in 0..H {
        for x in 0..W {
            if x < SAFE_MARGIN || y < SAFE_MARGIN || x >= W - SAFE_MARGIN || y >= H - SAFE_MARGIN {
                assert_eq!(f[((y * W + x) * 4) as usize], 255, "border touched at {x},{y}");
            }
        }
    }
}

#[test]
fn short_buffers_and_odd_sizes_never_panic_or_write_out_of_bounds() {
    let o = Overlay::new("T", Some("A"), None, MarkStyle::Sigil, true).unwrap();
    let mut tiny = vec![7u8; 10 * 10 * 4];
    o.draw(&mut tiny, 10, 10, 1.0);
    let mut short = vec![7u8; 100];
    o.draw(&mut short, 1920, 1080, 1.0);
    assert!(short.iter().all(|&b| b == 7));
}
