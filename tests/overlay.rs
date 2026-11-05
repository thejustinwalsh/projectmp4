#[path = "common/fonts.rs"]
mod fonts;
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
    // Fixed directory under target/tmp: rewritten each run, never one per process.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("overlay_fixtures");
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

#[test]
fn the_title_uses_the_embedded_oswald_not_a_system_font() {
    // Oracle: the same string through a FontSystem holding ONLY Oswald. Production also
    // has host fonts in its db, so if it resolves "Oswald" by name the pixels are
    // identical; any other family (see the positive control) differs.
    // Smallest wrong impl that passes: none short of pixel-identical Oswald Bold 34px,
    // except a wrong size applied to both sides (shared code path).
    let mine = fonts::render_title_ink("Hollow Signal");
    let oracle = fonts::oswald_only_ink("Hollow Signal");
    assert!(fonts::ink(&oracle) > 500, "oracle drew almost nothing: {}", fonts::ink(&oracle));
    let diff = fonts::mean_abs_diff(&mine, &oracle);
    assert!(diff < 1e-9, "title is not being shaped with Oswald (mean |diff| {diff})");
    assert_eq!((mine.w, mine.h), (oracle.w, oracle.h), "title tile size differs from Oswald's");
    // Golden size: production and oracle share the size/weight code, so a wrong font size
    // would be identical on both sides and invisible above. 207x61 is Oswald Bold 34px for
    // "Hollow Signal" plus the halo pad. If this fails after a cosmic-text bump or a change
    // to the font files, refresh it; if neither changed, the title size has regressed.
    assert_eq!((mine.w, mine.h), (207, 61), "golden title tile size changed");
}

#[test]
fn positive_control_a_different_family_is_far_from_the_oswald_oracle() {
    // If this were not large, the identity test above could not tell Oswald from anything.
    // Courier New is a macOS system face and certainly not Oswald. NOTE: it is resolved
    // from the host, so on a non-macOS machine this test fails loudly ("control family ... is
    // not installed"): that means it needs macOS fonts, not that the code regressed.
    // (The decoy test below reads /System/Library/Fonts/Supplemental by path, same caveat.)
    // Measured family difference is 0.192 at Bold on both sides, so 0.10 leaves headroom. It also serves as the
    // red run for "not pinning the family".
    let oracle = fonts::oswald_only_ink("Hollow Signal");
    let other = fonts::render_in_family("Courier New", "Hollow Signal");
    assert!(fonts::ink(&other) > 500, "control drew nothing");
    let diff = fonts::mean_abs_diff(&other, &oracle);
    assert!(diff > 0.10, "a different family is too close to Oswald: {diff}");
}

#[test]
fn a_host_installed_oswald_does_not_beat_the_embedded_one() {
    // Plant a decoy (Courier New Bold) under the family name "Oswald" as if the user had their
    // own Oswald installed. Smallest wrong impl that passes: none that loads host fonts first
    // (then the decoy wins and the diff is large); the decoy proved to win in that order.
    let decoy_first_would_be = fonts::render_in_family("Courier New", "Hollow Signal");
    let oracle = fonts::oswald_only_ink("Hollow Signal");
    assert!(fonts::mean_abs_diff(&decoy_first_would_be, &oracle) > 0.05, "decoy is not distinguishable from Oswald");
    let mine = fonts::title_with_decoy_host_oswald("Hollow Signal");
    let diff = fonts::mean_abs_diff(&mine, &oracle);
    assert!(diff < 1e-9, "a host face named Oswald beat the embedded one (mean |diff| {diff})");
}

#[test]
fn cyrillic_titles_render_from_oswald_itself() {
    // Oswald covers Cyrillic: no fallback needed, so this must match the oracle.
    let mine = fonts::render_title_ink("Тест Сигнал");
    let oracle = fonts::oswald_only_ink("Тест Сигнал");
    assert!(fonts::ink(&oracle) > 500, "oracle drew almost nothing: Oswald lacks Cyrillic?");
    assert!(fonts::ink(&mine) > 500, "Cyrillic title drew nothing");
    let diff = fonts::mean_abs_diff(&mine, &oracle);
    assert!(diff < 1e-9, "Cyrillic did not come from Oswald (mean |diff| {diff})");
}

#[test]
fn glyphs_oswald_lacks_fall_back_to_real_distinct_glyphs() {
    // Oswald has no CJK. Production must differ from the Oswald-only oracle (fallback live),
    // and the three characters must render as three *different* shapes: a host face that
    // only supplied its .notdef box would draw 日本語 and 日日日 identically.
    // Needs a host CJK font (macOS ships several); on a CJK-less CI machine mark this #[ignore].
    let mine = fonts::render_title_ink("日本語");
    let oracle = fonts::oswald_only_ink("日本語");
    assert!(fonts::ink(&mine) > 300, "CJK title drew almost nothing: {}", fonts::ink(&mine));
    assert!(fonts::mean_abs_diff(&mine, &oracle) > 1e-4, "CJK rendered like an Oswald-only db: no fallback");
    let same = fonts::render_title_ink("日日日");
    let d = fonts::mean_abs_diff(&mine, &same);
    assert!(d > 1e-3, "日本語 and 日日日 render alike (diff {d}): fallback is drawing .notdef boxes");
}

#[test]
fn overlay_draws_the_oswald_title_pixel_for_pixel() {
    // Overlay's own pixels vs the Oswald-only oracle. On a black frame the scrim and
    // halo are black, so the frame's RGB equals the text tile's premultiplied RGB.
    // Smallest wrong impl that passes: none that changes the font, size, weight or position
    // of the title (the crop is placed from the layout constants).
    let oracle = fonts::oswald_only_ink("Hollow Signal");
    let mut f = black_frame(W, H);
    Overlay::new("Hollow Signal", None, None, MarkStyle::Word, true).unwrap().draw(&mut f, W, H, 0.0);
    let x0 = W - SAFE_MARGIN - BLOCK_PAD as u32 - oracle.w as u32;
    let y0 = SAFE_MARGIN + BLOCK_PAD as u32;
    let mut crop = fonts::TitleInk { w: oracle.w, h: oracle.h, px: vec![0; oracle.w * oracle.h * 4] };
    for y in 0..oracle.h {
        for x in 0..oracle.w {
            let i = (((y0 + y as u32) * W + x0 + x as u32) * 4) as usize;
            let o = (y * oracle.w + x) * 4;
            crop.px[o..o + 3].copy_from_slice(&f[i..i + 3]);
            crop.px[o + 3] = oracle.px[o + 3]; // frame alpha is opaque; compare colour only
        }
    }
    assert!(fonts::ink(&crop) > 500, "no title ink at the expected position");
    let diff = fonts::mean_abs_diff(&crop, &oracle);
    assert!(diff < 1e-9, "Overlay title differs from the Oswald oracle (mean |diff| {diff})");
}

#[test]
fn the_artist_line_is_oswald_regular_below_the_title() {
    // Artist shapes at Weight::NORMAL through the same path. Matching the oracle pins that the
    // artist is drawn in Oswald at the weight the oracle also uses (a non-Oswald artist differs).
    // It does NOT pin Weight::NORMAL itself: that is hardcoded in the shared render_text_with,
    // so production and oracle agree on it by construction. The extra height proves a second line.
    let mine = fonts::render_title_artist_ink("Hollow Signal", "GH0STF1R3");
    let oracle = fonts::oswald_only_ink_with("Hollow Signal", Some("GH0STF1R3"));
    let title_only = fonts::oswald_only_ink("Hollow Signal");
    assert!(mine.h > title_only.h, "no artist line: {} vs {}", mine.h, title_only.h);
    assert!(fonts::ink(&mine) > fonts::ink(&title_only) + 200, "artist drew no ink");
    let diff = fonts::mean_abs_diff(&mine, &oracle);
    assert!(diff < 1e-9, "artist line differs from Oswald Regular (mean |diff| {diff})");
}

// --- Layers: what the preview uploads to the GPU must be what `draw` blends ---

/// Every pixel of `layer` that has any alpha, as it lands on a transparent-black
/// frame (premultiplied source over black is the source), must be in `frame` at
/// the layer's position, and nowhere else may the frame be non-black.
fn assert_layers_are_what_was_drawn(frame: &[u8], layers: &[projectmp4::render::Layer<'_>]) {
    let mut expect = black_frame(W, H);
    for l in layers {
        for j in 0..l.h {
            for i in 0..l.w {
                let s = &l.px[(j * l.w + i) * 4..][..4];
                let (x, y) = (l.x + i as i64, l.y + j as i64);
                assert!(x >= 0 && y >= 0 && x < W as i64 && y < H as i64, "layer pixel outside the frame");
                if s[3] > 0 {
                    expect[((y as u32 * W + x as u32) * 4) as usize..][..3].copy_from_slice(&s[..3]);
                }
            }
        }
    }
    // Layers do not overlap here, so over-black is a plain copy.
    assert!(frame == expect.as_slice(), "the drawn frame differs from the layers");
}

#[test]
fn the_gpu_layers_are_the_pixels_draw_blends() {
    let o = Overlay::new("Layer Title", Some("An Artist"), None, MarkStyle::Word, true).unwrap();
    let bass = 0.7;
    let mut frame = black_frame(W, H);
    o.draw(&mut frame, W, H, bass);
    let block = o.block_layer(W, H).expect("a title makes a block");
    let mark = o.mark_layer(o.mark_index(bass), W, H);
    assert_layers_are_what_was_drawn(&frame, &[block, mark]);
}

#[test]
fn bass_selects_the_watermark_tile() {
    let o = Overlay::new("T", None, None, MarkStyle::Word, true).unwrap();
    let last = o.mark_layers(W, H).len() - 1;
    assert!(last > 0, "a reactive overlay has several tiles");
    assert_eq!(o.mark_index(0.0), 0);
    assert_eq!(o.mark_index(1.0), last);
    assert_eq!(o.mark_index(2.0), last, "clamped");
    assert_eq!(o.mark_index(-1.0), 0, "clamped");
    assert_eq!(o.mark_index(f32::NAN), 0);
    let mid = o.mark_index(0.5);
    assert!(mid > 0 && mid < last, "{mid} of {last}");
    assert!((0..=10).map(|i| o.mark_index(i as f32 / 10.0)).collect::<Vec<_>>().windows(2).all(|w| w[0] <= w[1]));

    let s = Overlay::new("T", None, None, MarkStyle::Word, false).unwrap();
    assert_eq!(s.mark_layers(W, H).len(), 1);
    assert_eq!(s.mark_index(1.0), 0, "a static overlay ignores bass");
}

#[test]
fn an_overlay_with_no_title_or_art_has_no_block_layer() {
    let o = Overlay::new("", None, None, MarkStyle::Word, true).unwrap();
    assert!(o.block_layer(W, H).is_none());
}
