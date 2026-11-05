//! Title-tile helpers: the production font path vs an Oswald-only oracle.
#![allow(dead_code)]
use cosmic_text::FontSystem;
use projectmp4::render::overlay::{FONT_FAMILY, font_system_with_host, production_font_system, text_tile_pixels, OSWALD_BOLD, OSWALD_REGULAR};

/// Premultiplied RGBA title tile.
pub struct TitleInk {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

fn tile(fs: &mut FontSystem, family: &str, title: &str, artist: Option<&str>) -> TitleInk {
    let (w, h, px) = text_tile_pixels(fs, family, title, artist).expect("text tile");
    TitleInk { w, h, px }
}

/// The title as the Overlay shapes it (embedded Oswald + host fonts for fallback).
pub fn render_title_ink(title: &str) -> TitleInk {
    tile(&mut production_font_system(), FONT_FAMILY, title, None)
}

/// Title and artist as the Overlay shapes them.
pub fn render_title_artist_ink(title: &str, artist: &str) -> TitleInk {
    tile(&mut production_font_system(), FONT_FAMILY, title, Some(artist))
}

/// Positive control: the production font system asked for a deliberately different
/// family. `family` must exist on the host, otherwise the control proves nothing.
pub fn render_in_family(family: &str, title: &str) -> TitleInk {
    let mut fs = production_font_system();
    let present = fs.db().faces().any(|f| f.families.iter().any(|(n, _)| n == family));
    assert!(present, "control family {family:?} is not installed on this host");
    tile(&mut fs, family, title, None)
}

/// A face that is NOT Oswald registered under the family name "Oswald", standing
/// in for a user who has their own (different) Oswald installed.
fn plant_decoy_oswald(db: &mut fontdb::Database) {
    let path = "/System/Library/Fonts/Supplemental/Courier New Bold.ttf";
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("decoy font {path}: {e}"));
    let mut scratch = fontdb::Database::new();
    scratch.load_font_data(bytes.clone());
    let mut info = scratch.faces().next().expect("decoy face").clone();
    info.families = vec![("Oswald".to_string(), fontdb::Language::English_UnitedStates)];
    info.weight = fontdb::Weight::BOLD;
    info.style = fontdb::Style::Normal;
    info.source = fontdb::Source::Binary(std::sync::Arc::new(bytes));
    db.push_face_info(info);
}

/// Production font-system construction, but with a decoy "Oswald" as the host's font.
pub fn title_with_decoy_host_oswald(title: &str) -> TitleInk {
    tile(&mut font_system_with_host(plant_decoy_oswald), FONT_FAMILY, title, None)
}

/// The same string through a FontSystem whose database holds ONLY the two Oswald faces.
pub fn oswald_only_ink(title: &str) -> TitleInk {
    oswald_only_ink_with(title, None)
}

pub fn oswald_only_ink_with(title: &str, artist: Option<&str>) -> TitleInk {
    let mut db = fontdb::Database::new();
    db.load_font_data(OSWALD_REGULAR.to_vec());
    db.load_font_data(OSWALD_BOLD.to_vec());
    assert_eq!(db.len(), 2, "oracle db must hold exactly Oswald Regular and Bold");
    tile(&mut FontSystem::new_with_locale_and_db("en-US".into(), db), "Oswald", title, artist)
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
