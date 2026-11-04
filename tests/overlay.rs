use projectmp4::render::overlay::{ART_PX, BLOCK_PAD, SAFE_MARGIN};
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

/// `black_outside`: columns outside this x range are painted black, so only a
/// true centre crop (not a squash) yields a fully inked 180px square.
fn fixture(name: &str, w: u32, h: u32, keep: Option<(u32, u32)>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("projectmp4-overlay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    // Every channel >= 60 so every art pixel counts as ink.
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        if keep.is_some_and(|(a, b)| x < a || x >= b) {
            return image::Rgb([0, 0, 0]);
        }
        image::Rgb([(60 + x * 190 / w) as u8, (60 + y * 190 / h) as u8, 220])
    });
    img.save(&path).unwrap();
    path
}
// Tests run in parallel: write each fixture exactly once, never while another test reads it.
fn art_16x9() -> PathBuf {
    static P: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    P.get_or_init(|| fixture("art16x9.png", 640, 360, Some((140, 500)))).clone()
}
fn art_1x1() -> PathBuf {
    static P: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    P.get_or_init(|| fixture("art1x1.png", 360, 360, None)).clone()
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
    // The 16:9 fixture is black outside its centre 360px: a squash to 180x180
    // would leave ~44% of the art black, only a centre crop inks all of it.
    let text_only = ink_in_region(&render(None, 0.0), Region::UpperRight);
    let wide = drawn_with(art_16x9());
    let delta = ink_in_region(&wide, Region::UpperRight) - text_only;
    assert!(delta >= 0.98 * (ART_PX * ART_PX) as f64, "art ink {delta}: not a centre crop");

    let a = bbox_of(Region::UpperRight, &wide).expect("16:9 render drew nothing");
    assert!(a.2 - a.0 > ART_PX && a.3 - a.1 >= ART_PX - 1, "box too small to contain the art: {a:?}");
    assert_eq!(Some(a), bbox_of(Region::UpperRight, &drawn_with(art_1x1())));
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
    // Linear 1.2 => area 1.44 (measured ~1.49); 1.01 or 1.5 must both fail.
    assert!(loud > quiet * 1.3 && loud < quiet * 1.6, "quiet {quiet}, loud {loud}");
}

fn white_frame() -> Vec<u8> {
    [255u8, 255, 255, 255].repeat((W * H) as usize)
}

fn lum_at(f: &[u8], x: u32, y: u32) -> u8 {
    f[((y * W + x) * 4) as usize]
}

#[test]
fn halo_reaches_about_six_pixels_and_no_further_than_the_pad() {
    let o = Overlay::new("T", None, None, MarkStyle::Word, true).unwrap();
    let mut black = black_frame(W, H);
    o.draw(&mut black, W, H, 0.0);
    let mut white = white_frame();
    o.draw(&mut white, W, H, 0.0);

    // Chebyshev distance rings around the mark's ink, in the lower-left window.
    let (x0, y0, x1, y1) = (SAFE_MARGIN, H - SAFE_MARGIN - 120, SAFE_MARGIN + 300, H - SAFE_MARGIN);
    let idx = |x: u32, y: u32| ((y - y0) * (x1 - x0) + (x - x0)) as usize;
    let mut reach = vec![false; ((x1 - x0) * (y1 - y0)) as usize];
    for y in y0..y1 {
        for x in x0..x1 {
            reach[idx(x, y)] = is_ink(&black, x, y);
        }
    }
    assert!(reach.iter().any(|&b| b), "mark not in the probe window");
    let mut ring: Vec<Vec<(u32, u32)>> = Vec::new();
    for d in 0..=12 {
        if d > 0 {
            let prev = reach.clone();
            let mut cells = Vec::new();
            for y in y0..y1 {
                for x in x0..x1 {
                    if prev[idx(x, y)] {
                        continue;
                    }
                    let near = (-1i32..=1).any(|dy| {
                        (-1i32..=1).any(|dx| {
                            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                            nx >= x0 as i32 && nx < x1 as i32 && ny >= y0 as i32 && ny < y1 as i32 && prev[idx(nx as u32, ny as u32)]
                        })
                    });
                    if near {
                        reach[idx(x, y)] = true;
                        cells.push((x, y));
                    }
                }
            }
            ring.push(cells);
        } else {
            ring.push(Vec::new());
        }
    }
    let darkened = |d: usize| ring[d].iter().filter(|&&(x, y)| lum_at(&white, x, y) < 250).count() as f64 / ring[d].len() as f64;
    let d6 = darkened(6);
    assert!(d6 > 0.5, "only {:.0}% of pixels 6px from ink are darkened", d6 * 100.0);
    assert!(ring[12].iter().all(|&(x, y)| lum_at(&white, x, y) == 255), "halo reaches 12px, past its pad");
}

#[test]
fn white_frame_border_is_untouched_with_art_at_every_bass() {
    // A black halo/scrim is invisible on black, so the border is checked on white.
    let art = art_1x1();
    for bass in [0.0, 0.5, 1.0] {
        let mut f = white_frame();
        Overlay::new("Hollow Signal", Some("GH0STF1R3"), Some(&art), MarkStyle::Word, true)
            .unwrap()
            .draw(&mut f, W, H, bass);
        assert!(f.iter().any(|&b| b < 255), "nothing drawn");
        for y in 0..H {
            for x in 0..W {
                if x < SAFE_MARGIN || y < SAFE_MARGIN || x >= W - SAFE_MARGIN || y >= H - SAFE_MARGIN {
                    assert_eq!(lum_at(&f, x, y), 255, "bass {bass}: border touched at {x},{y}");
                }
            }
        }
    }
}

#[test]
fn scrim_covers_the_art_tile_not_just_the_text() {
    let mut f = white_frame();
    Overlay::new("Hollow Signal", Some("GH0STF1R3"), Some(&art_1x1()), MarkStyle::Word, true)
        .unwrap()
        .draw(&mut f, W, H, 0.0);
    let (ax1, ay0) = (W - SAFE_MARGIN - BLOCK_PAD as u32, SAFE_MARGIN + BLOCK_PAD as u32);
    let (ax0, ay1) = (ax1 - ART_PX, ay0 + ART_PX);
    assert!(lum_at(&f, (ax0 + ax1) / 2, (ay0 + ay1) / 2) < 255, "art is not where the layout says");
    let mean = |x0: u32, y0: u32, x1: u32, y1: u32| {
        let mut s = 0u64;
        for y in y0..y1 {
            for x in x0..x1 {
                s += lum_at(&f, x, y) as u64;
            }
        }
        s as f64 / ((x1 - x0) * (y1 - y0)) as f64
    };
    // Bare background is 255. Each 8px band hugging the art must be clearly darkened.
    for (name, m) in [
        ("above", mean(ax0, ay0 - 8, ax1, ay0)),
        ("below", mean(ax0, ay1, ax1, ay1 + 8)),
        ("right", mean(ax1, ay0, ax1 + 8, ay1)),
    ] {
        assert!(m < 180.0, "band {name} the art is {m:.0}/255: no scrim behind the art tile");
    }
}

#[test]
fn short_buffers_and_odd_sizes_never_panic_or_write_out_of_bounds() {
    let o = Overlay::new("T", Some("A"), None, MarkStyle::Sigil, true).unwrap();
    // 10x10: every element is off-frame, so nothing may change.
    let mut tiny = vec![7u8; 10 * 10 * 4];
    o.draw(&mut tiny, 10, 10, 1.0);
    assert!(tiny.iter().all(|&b| b == 7));
    let mut short = vec![7u8; 100];
    o.draw(&mut short, 1920, 1080, 1.0);
    assert!(short.iter().all(|&b| b == 7));
    // Dimensions whose product wraps must be rejected, not sliced out of bounds.
    let mut one = vec![7u8; 16];
    o.draw(&mut one, 1 << 31, 1 << 31, 1.0);
    assert!(one.iter().all(|&b| b == 7));

    // 300x200: partial overlap. The 1080p-sized block and mark are larger than
    // this frame's safe area, so only the top edge (block top and mark top both
    // sit below it) is guaranteed; the rest is clipped to the buffer, not rescaled.
    // (The sigil is 84px tall, taller than a 200px frame's 72px safe height, so use the word.)
    let o = Overlay::new("T", Some("A"), None, MarkStyle::Word, true).unwrap();
    let (w, h, m) = (300u32, 200u32, SAFE_MARGIN);
    let mut f = vec![7u8; (w * h * 4) as usize];
    o.draw(&mut f, w, h, 1.0);
    assert_eq!(f.len(), (w * h * 4) as usize);
    assert!(f.iter().any(|&b| b != 7), "nothing drawn at 300x200");
    for y in 0..h {
        for x in 0..w {
            if y < m {
                let i = ((y * w + x) * 4) as usize;
                assert_eq!(f[i..i + 4], [7, 7, 7, 7], "border touched at {x},{y}");
            }
        }
    }
}
