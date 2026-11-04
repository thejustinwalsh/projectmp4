//! Shared by `tests/mark.rs` and `examples/mark_sheet.rs`.
#![allow(dead_code)]

pub const MARK_SVG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/gh0stf1r3.svg");

pub struct Raster {
    pub w: u32,
    pub h: u32,
    /// Straight-order RGBA bytes as stored by tiny-skia (premultiplied).
    pub px: Vec<[u8; 4]>,
    pub pixmap: resvg::tiny_skia::Pixmap,
}

/// The `viewBox` of `<symbol id="...">`, or a panic naming the missing id.
pub fn symbol_viewbox(svg: &str, id: &str) -> [f64; 4] {
    let needle = format!("<symbol id=\"{id}\"");
    let start = svg
        .find(&needle)
        .unwrap_or_else(|| panic!("symbol id {id:?} does not exist in the SVG"));
    let tag = &svg[start..svg[start..].find('>').unwrap() + start];
    let vb = tag
        .split("viewBox=\"")
        .nth(1)
        .unwrap_or_else(|| panic!("symbol {id:?} has no viewBox"))
        .split('"')
        .next()
        .unwrap();
    let n: Vec<f64> = vb.split_whitespace().map(|v| v.parse().unwrap()).collect();
    assert_eq!(n.len(), 4, "bad viewBox {vb:?}");
    [n[0], n[1], n[2], n[3]]
}

/// Rasterise `<symbol id>` so that 1000 viewBox units (the cap height) span
/// `cap_px` pixels. Canvas size comes from the symbol's own viewBox.
pub fn render_svg_symbol(path: &str, id: &str, cap_px: u32, ink: &str) -> Raster {
    let svg = std::fs::read_to_string(path).unwrap();
    let [vx, vy, vw, vh] = symbol_viewbox(&svg, id);
    let scale = cap_px as f64 / 1000.0;
    let (w, h) = ((vw * scale).round() as u32, (vh * scale).round() as u32);

    let root_end = svg.find('>').unwrap();
    assert!(svg.starts_with("<svg"), "unexpected root");
    let body_end = svg.rfind("</svg>").unwrap();
    let doc = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{vw}\" height=\"{vh}\" \
         viewBox=\"{vx} {vy} {vw} {vh}\" color=\"{ink}\">{}<use href=\"#{id}\" x=\"{vx}\" y=\"{vy}\" \
         width=\"{vw}\" height=\"{vh}\"/></svg>",
        &svg[root_end + 1..body_end]
    );

    let tree = resvg::usvg::Tree::from_str(&doc, &resvg::usvg::Options::default()).unwrap();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).unwrap();
    let t = resvg::tiny_skia::Transform::from_scale(w as f32 / vw as f32, h as f32 / vh as f32);
    resvg::render(&tree, t, &mut pixmap.as_mut());
    let px = pixmap.data().chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
    Raster { w, h, px, pixmap }
}

/// Fraction of pixels with non-zero alpha.
pub fn ink_coverage(px: &[[u8; 4]]) -> f64 {
    px.iter().filter(|p| p[3] > 0).count() as f64 / px.len() as f64
}

/// Bounding box of non-zero-alpha pixels as (width, height) fractions of the canvas.
pub fn ink_bbox_fraction(r: &Raster) -> (f64, f64) {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (i, p) in r.px.iter().enumerate() {
        if p[3] > 0 {
            let (x, y) = (i as u32 % r.w, i as u32 / r.w);
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
    }
    if x0 == u32::MAX {
        return (0.0, 0.0);
    }
    ((x1 - x0 + 1) as f64 / r.w as f64, (y1 - y0 + 1) as f64 / r.h as f64)
}

/// 16x16 grid of mean alpha (0..1), resolution independent, for shape comparison.
pub fn shape_signature(r: &Raster) -> Vec<f64> {
    const G: usize = 16;
    let mut sum = vec![0.0; G * G];
    let mut cnt = vec![0.0; G * G];
    for (i, p) in r.px.iter().enumerate() {
        let (x, y) = (i % r.w as usize, i / r.w as usize);
        let c = (y * G / r.h as usize) * G + x * G / r.w as usize;
        sum[c] += p[3] as f64 / 255.0;
        cnt[c] += 1.0;
    }
    sum.iter().zip(&cnt).map(|(s, n)| if *n > 0.0 { s / n } else { 0.0 }).collect()
}

/// Mean absolute difference between two signatures.
pub fn signature_distance(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f64>() / a.len() as f64
}
