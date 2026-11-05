//! Title-tile helpers: the production font path vs an Oswald-only oracle.
#![allow(dead_code)]
use cosmic_text::FontSystem;
use projectmp4::render::overlay::{production_font_system, title_tile_pixels, OSWALD_BOLD, OSWALD_REGULAR};

/// Premultiplied RGBA title tile.
pub struct TitleInk {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

fn tile(fs: &mut FontSystem, title: &str) -> TitleInk {
    let (w, h, px) = title_tile_pixels(fs, title).expect("title tile");
    TitleInk { w, h, px }
}

/// The title as the Overlay shapes it (embedded Oswald + host fonts for fallback).
pub fn render_title_ink(title: &str) -> TitleInk {
    tile(&mut production_font_system(), title)
}

/// The same string through a FontSystem whose database holds ONLY the two Oswald faces.
pub fn oswald_only_ink(title: &str) -> TitleInk {
    let mut db = fontdb::Database::new();
    db.load_font_data(OSWALD_REGULAR.to_vec());
    db.load_font_data(OSWALD_BOLD.to_vec());
    assert_eq!(db.len(), 2, "oracle db must hold exactly Oswald Regular and Bold");
    tile(&mut FontSystem::new_with_locale_and_db("en-US".into(), db), title)
}

/// Pixels with visible (non-halo) ink: halo is pure black, ink is white.
pub fn ink(t: &TitleInk) -> usize {
    t.px.chunks_exact(4).filter(|p| p[0].max(p[1]).max(p[2]) > 24).count()
}

/// Mean absolute byte difference (0..1) over the union of both tiles, aligned at
/// the top-left; missing area counts as transparent. A size mismatch therefore
/// shows up as a real pixel difference rather than a separate error.
pub fn mean_abs_diff(a: &TitleInk, b: &TitleInk) -> f64 {
    let (w, h) = (a.w.max(b.w), a.h.max(b.h));
    let at = |t: &TitleInk, x: usize, y: usize, c: usize| if x < t.w && y < t.h { t.px[(y * t.w + x) * 4 + c] as f64 } else { 0.0 };
    let mut s = 0.0;
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                s += (at(a, x, y, c) - at(b, x, y, c)).abs();
            }
        }
    }
    s / (w * h * 4) as f64 / 255.0
}
