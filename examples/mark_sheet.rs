//! Renders both mark symbols at 28/56/112 px cap height over black and over a
//! hostile saturated noise field into target/mark-sheet.png, and asserts the
//! properties one would otherwise judge by eye.
#[path = "../tests/common/mark.rs"]
mod mark;
use mark::*;
use resvg::tiny_skia::{Color, Pixmap, PixmapPaint, Transform};

const SIZES: [u32; 3] = [28, 56, 112];
const PAD: u32 = 16;

/// Deterministic hash noise -> fully saturated random-hue pixels, blocky at 3px
/// so it has both high-frequency and mid-frequency energy.
fn hostile_field(w: u32, h: u32) -> Pixmap {
    let mut pm = Pixmap::new(w, h).unwrap();
    let hash = |x: u32, y: u32| {
        let mut v = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B) ^ 0xC2B2_AE35;
        v ^= v >> 15;
        v = v.wrapping_mul(0x2C1B_3C6D);
        v ^= v >> 12;
        v = v.wrapping_mul(0x297A_2D39);
        v ^ (v >> 15)
    };
    for y in 0..h {
        for x in 0..w {
            let hue = (hash(x / 3, y / 3) % 360) as f32;
            let k = |n: f32| {
                let k = (n + hue / 60.0) % 6.0;
                1.0 - k.min(4.0 - k).clamp(0.0, 1.0)
            };
            let (r, g, b) = (k(5.0), k(3.0), k(1.0));
            let i = ((y * w + x) * 4) as usize;
            pm.data_mut()[i..i + 4]
                .copy_from_slice(&[(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, 255]);
        }
    }
    pm
}

fn main() {
    let ids = ["word", "sigil"];
    let rasters: Vec<Vec<Raster>> = ids
        .iter()
        .map(|id| SIZES.iter().map(|&s| render_svg_symbol(MARK_SVG, id, s, "white")).collect())
        .collect();

    println!("{:<6} {:>4} {:>9} {:>8} {:>16}", "symbol", "cap", "canvas", "ink", "bbox w x h");
    for (id, row) in ids.iter().zip(&rasters) {
        for (r, s) in row.iter().zip(SIZES) {
            let cov = ink_coverage(&r.px);
            let (bw, bh) = ink_bbox_fraction(r);
            println!("{id:<6} {s:>4} {:>4}x{:<4} {cov:>8.4} {bw:>7.2} x {bh:.2}", r.w, r.h);
            assert!(cov > 0.04 && cov < 0.60, "{id}@{s}: coverage {cov:.3} out of band");
            assert!(bw > 0.75 && bh > 0.75, "{id}@{s}: ink does not span the canvas");
        }
    }

    // Layout: one row per (symbol, size); left panel black, right panel noise.
    let cw = rasters[0][2].w.max(rasters[1][2].w) + PAD * 2;
    let rows: Vec<(usize, usize)> = (0..2).flat_map(|s| (0..3).map(move |z| (s, z))).collect();
    let row_h: Vec<u32> = rows.iter().map(|&(s, z)| rasters[s][z].h + PAD * 2).collect();
    let (w, h) = (cw * 2, row_h.iter().sum::<u32>());
    let mut sheet = Pixmap::new(w, h).unwrap();
    let mut panel = Pixmap::new(cw, h).unwrap();
    panel.fill(Color::BLACK);
    sheet.draw_pixmap(0, 0, panel.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    let noise = hostile_field(cw, h);
    sheet.draw_pixmap(cw as i32, 0, noise.as_ref(), &PixmapPaint::default(), Transform::identity(), None);

    let mut y = 0;
    for (&(s, z), rh) in rows.iter().zip(&row_h) {
        for px0 in [0, cw] {
            sheet.draw_pixmap(
                (px0 + PAD) as i32,
                (y + PAD) as i32,
                rasters[s][z].pixmap.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
        y += rh;
    }

    // Contrast of bare white ink over the noise (no halo: that is Task 10).
    // Mean luminance of the noise under ink, to show how hostile the field is.
    let lum = |p: &[u8]| 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64;
    let mean_noise = noise.data().chunks_exact(4).map(lum).sum::<f64>() / (cw * h) as f64;
    println!("noise field mean luminance {mean_noise:.0}/255 (white ink, no halo)");

    std::fs::create_dir_all("target").unwrap();
    sheet.save_png("target/mark-sheet.png").unwrap();
    println!("wrote target/mark-sheet.png ({w}x{h})");
}
