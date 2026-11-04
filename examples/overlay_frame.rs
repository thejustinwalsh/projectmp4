//! Composites the overlay onto a full 1920x1080 frame of saturated noise (and
//! onto black), writes target/overlay-frame.png, prints per-region ink numbers
//! and asserts what one would otherwise judge by eye.
use projectmp4::render::overlay::{ART_PX, SAFE_MARGIN};
use projectmp4::render::{MarkStyle, Overlay};

const W: u32 = 1920;
const H: u32 = 1080;

fn hash(x: u32, y: u32) -> u32 {
    let mut v = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B) ^ 0xC2B2_AE35;
    v ^= v >> 15;
    v = v.wrapping_mul(0x2C1B_3C6D);
    v ^= v >> 12;
    v = v.wrapping_mul(0x297A_2D39);
    v ^ (v >> 15)
}

/// Bright, fully saturated random-hue blocks; mean luminance is high.
fn noise_frame() -> Vec<u8> {
    let mut f = vec![255u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let hue = (hash(x / 3, y / 3) % 360) as f32;
            let k = |n: f32| {
                let k = (n + hue / 60.0) % 6.0;
                1.0 - k.min(4.0 - k).clamp(0.0, 1.0)
            };
            let i = ((y * W + x) * 4) as usize;
            f[i] = (k(5.0) * 255.0) as u8;
            f[i + 1] = (k(3.0) * 255.0) as u8;
            f[i + 2] = (k(1.0) * 255.0) as u8;
        }
    }
    f
}

fn lum(f: &[u8], i: usize) -> f64 {
    0.2126 * f[i] as f64 + 0.7152 * f[i + 1] as f64 + 0.0722 * f[i + 2] as f64
}

/// (ink pixels over black, mean luminance of the non-ink pixels that touch ink
/// over noise). Ink pixels are those that are >= 200 luminance after compositing
/// AND brighter than the same pixel on the bare noise is not required: white
/// ink pixels are identified from the black-frame render.
fn region_report(name: &str, rect: (u32, u32, u32, u32), black: &[u8], noisy: &[u8], bare: &[u8]) -> (usize, f64, f64) {
    let (x0, y0, x1, y1) = rect;
    let mut ink = vec![false; (W * H) as usize];
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * W + x) * 4) as usize;
            if black[i].max(black[i + 1]).max(black[i + 2]) > 200 {
                ink[(y * W + x) as usize] = true;
                n += 1;
            }
        }
    }
    // Surround: non-ink pixels within 3px of ink; compare noisy vs bare noise.
    let (mut s_now, mut s_bare, mut cnt) = (0.0, 0.0, 0usize);
    for y in y0.max(3)..y1.min(H - 3) {
        for x in x0.max(3)..x1.min(W - 3) {
            if ink[(y * W + x) as usize] {
                continue;
            }
            let near = (-3i32..=3).any(|dy| (-3i32..=3).any(|dx| ink[((y as i32 + dy) as u32 * W + (x as i32 + dx) as u32) as usize]));
            if near {
                let i = ((y * W + x) * 4) as usize;
                s_now += lum(noisy, i);
                s_bare += lum(bare, i);
                cnt += 1;
            }
        }
    }
    let (now, bare_m) = (s_now / cnt.max(1) as f64, s_bare / cnt.max(1) as f64);
    println!("{name:<28} ink px {n:>6}   surround luminance {bare_m:>5.0} -> {now:>5.0} (white ink = 255)");
    (n, now, bare_m)
}

fn main() {
    // Cover art: a bright gradient so the art square counts as ink.
    let art = std::env::temp_dir().join(format!("projectmp4-overlay-frame-{}.png", std::process::id()));
    image::RgbImage::from_fn(640, 360, |x, y| image::Rgb([(60 + x * 190 / 640) as u8, (60 + y * 190 / 360) as u8, 220]))
        .save(&art)
        .unwrap();

    let overlay = Overlay::new("Hollow Signal", Some("GH0STF1R3"), Some(&art), MarkStyle::Word, true).unwrap();
    let bare = noise_frame();
    let mut noisy = bare.clone();
    overlay.draw(&mut noisy, W, H, 0.0);
    let mut black = [0u8, 0, 0, 255].repeat((W * H) as usize);
    overlay.draw(&mut black, W, H, 0.0);

    let mean = |f: &[u8]| (0..(W * H) as usize).map(|p| lum(f, p * 4)).sum::<f64>() / (W * H) as f64;
    println!("noise field mean luminance {:.0}/255\n", mean(&bare));

    // Upper-right block region = text + art.
    let ur = (W / 2, 0, W, H / 2);
    let ll = (0, H / 2, W / 2, H);
    let report = |name: &str, o: &Overlay, bass: f32, rect| {
        let mut bk = [0u8, 0, 0, 255].repeat((W * H) as usize);
        o.draw(&mut bk, W, H, bass);
        let mut nz = bare.clone();
        o.draw(&mut nz, W, H, bass);
        region_report(name, rect, &bk, &nz, &bare)
    };
    let text_overlay = Overlay::new("Hollow Signal", Some("GH0STF1R3"), None, MarkStyle::Word, true).unwrap();
    let (text_ink, text_now, text_bare) = report("upper right, text only", &text_overlay, 0.0, ur);
    let (ur_ink, _, _) = report("upper right, text + art", &overlay, 0.0, ur);
    let (ll_ink, mark_now, mark_bare) = report("lower left, watermark", &overlay, 0.0, ll);
    let (ll_loud, _, _) = report("lower left, bass 1.0", &overlay, 1.0, ll);

    assert!((1500..30000).contains(&text_ink), "text ink {text_ink}");
    assert!(ur_ink as f64 >= text_ink as f64 + 0.98 * (ART_PX * ART_PX) as f64, "art did not draw");
    assert!((600..5000).contains(&ll_ink), "watermark ink {ll_ink}");
    assert!(ll_loud > ll_ink && (ll_loud as f64) < ll_ink as f64 * 1.6);
    // Halo: ink's immediate surround must be much darker than the bare field.
    assert!(mark_now < mark_bare * 0.5, "watermark halo too weak: {mark_bare:.0} -> {mark_now:.0}");
    assert!(text_now < text_bare * 0.5, "text halo too weak: {text_bare:.0} -> {text_now:.0}");
    // White ink (255) against its surround: a contrast ratio, WCAG-style on luminance/255.
    let cr = |l: f64| (1.0 + 0.05) / (l / 255.0 + 0.05);
    println!("\ncontrast of white ink vs surround: mark {:.1}:1 (bare {:.1}:1), text {:.1}:1 (bare {:.1}:1)",
        cr(mark_now), cr(mark_bare), cr(text_now), cr(text_bare));
    assert!(cr(mark_now) > 3.0 && cr(text_now) > 3.0, "ink does not clear 3:1 over the noise");

    // Safe margin on the noisy frame: border must equal the bare field exactly.
    let mut touched = 0;
    for y in 0..H {
        for x in 0..W {
            if x < SAFE_MARGIN || y < SAFE_MARGIN || x >= W - SAFE_MARGIN || y >= H - SAFE_MARGIN {
                let i = ((y * W + x) * 4) as usize;
                touched += (noisy[i..i + 4] != bare[i..i + 4]) as usize;
            }
        }
    }
    println!("pixels changed inside the {SAFE_MARGIN}px border: {touched}");
    assert_eq!(touched, 0);

    std::fs::create_dir_all("target").unwrap();
    image::RgbaImage::from_raw(W, H, noisy).unwrap().save("target/overlay-frame.png").unwrap();
    println!("wrote target/overlay-frame.png");
}
