//! Title, artist and cover art in the upper right; the GH0STF1R3 mark in the
//! lower left. Everything is pre-rasterised into premultiplied RGBA tiles at
//! construction; `draw` only alpha-blends tiles, clipped to the buffer.
//!
//! Legibility over unpredictable visuals comes from two layers, both baked into
//! the tiles: a feathered dark scrim behind the title block, and a soft dark
//! halo (blurred, boosted copy of each ink alpha) under the text and the mark.
//! Halo and scrim are pure black so they are invisible on a black frame.
//!
//! Every tile is anchored with its padded corner on the 64 px safe margin, and
//! padding is at least the halo radius, so no layer reaches the margin.

use super::mark::rasterise_symbol;
use crate::ffi::{Error, Result};
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
use std::path::Path;

pub const SAFE_MARGIN: u32 = 64;
pub const ART_PX: u32 = 180;
/// Watermark growth at full bass, as a *linear* scale. Ink area grows with the
/// square (1.2^2 = 1.44), which is what keeps loud/quiet ink under 1.6x.
pub const MAX_MARK_SCALE: f32 = 1.2;

/// Embedded typeface (SIL OFL 1.1, see assets/fonts).
///
/// Two facts make the title identical across machines, both established by
/// experiment (a host face registered under the family name "Oswald"):
/// 1. the face is requested by *name* (`Family::Name`), never a generic family
///    such as `SansSerif`, which resolved to different pixels than Oswald on the one host
///    measured (not verified on others); and
/// 2. the embedded faces are inserted into the database *before* the host's, because
///    when two faces share a family and weight, the first one inserted wins. (cosmic-text's
///    own `new_with_fonts` loads system fonts first, so a host-installed Oswald
///    would beat the embedded copy.) The host fonts stay in the database only
///    as fallback for glyphs Oswald lacks (CJK, Arabic, ...).
#[doc(hidden)]
pub const OSWALD_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Oswald-Regular.ttf");
#[doc(hidden)]
pub const OSWALD_BOLD: &[u8] = include_bytes!("../../assets/fonts/Oswald-Bold.ttf");
#[doc(hidden)]
pub const FONT_FAMILY: &str = "Oswald";

/// Embedded Oswald first, then `host` (production: the system fonts), then
/// the platform fallback. `host` is a parameter so tests can plant a decoy
/// host "Oswald" and prove the embedded copy still wins.
#[doc(hidden)]
pub fn font_system_with_host(host: impl FnOnce(&mut fontdb::Database)) -> FontSystem {
    let mut db = fontdb::Database::new();
    db.load_font_data(OSWALD_REGULAR.to_vec());
    db.load_font_data(OSWALD_BOLD.to_vec());
    host(&mut db);
    // The locale only steers script-specific fallback ordering.
    FontSystem::new_with_locale_and_db("en-US".to_string(), db)
}

#[doc(hidden)]
pub fn production_font_system() -> FontSystem {
    font_system_with_host(|db| db.load_system_fonts())
}

/// Text tile as (width, height, premultiplied RGBA) shaped with `fs` in
/// `family`. Exposed so tests can compare against an Oswald-only oracle and
/// run a deliberately different family as a positive control.
#[doc(hidden)]
pub fn text_tile_pixels(fs: &mut FontSystem, family: &str, title: &str, artist: Option<&str>) -> Result<(usize, usize, Vec<u8>)> {
    let t = render_text_with(fs, family, title, artist)?;
    Ok((t.w, t.h, t.px))
}

/// Title-only convenience over [`text_tile_pixels`] with the production family.
#[doc(hidden)]
pub fn title_tile_pixels(fs: &mut FontSystem, title: &str) -> Result<(usize, usize, Vec<u8>)> {
    text_tile_pixels(fs, FONT_FAMILY, title, None)
}

const MARK_SVG: &str = include_str!("../../assets/gh0stf1r3.svg");
const WORD_CAP_PX: f32 = 36.0;
const SIGIL_CAP_PX: f32 = 48.0;
const HALO_RADIUS: usize = 6;
const HALO_PAD: usize = 10;
const SCALE_LEVELS: usize = 16;
pub const BLOCK_PAD: usize = 24;
const BLOCK_GAP: usize = 20;
const MAX_TEXT_CHARS: usize = 200;
const TEXT_MAX_W: f32 = 640.0;
const SCRIM_FEATHER: f32 = 20.0;
const SCRIM_ALPHA: f32 = 0.6;
const MAX_ART_FILE_BYTES: u64 = 64 << 20;
const MAX_ART_DECODE_BYTES: u64 = 256 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkStyle {
    Word,
    Sigil,
}

impl MarkStyle {
    fn symbol(self) -> &'static str {
        match self {
            MarkStyle::Word => "word",
            MarkStyle::Sigil => "sigil",
        }
    }
    fn cap_px(self) -> f32 {
        match self {
            MarkStyle::Word => WORD_CAP_PX,
            MarkStyle::Sigil => SIGIL_CAP_PX,
        }
    }
}

/// Premultiplied RGBA8 tile.
#[derive(Clone)]
struct Tile {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Tile {
    fn new(w: usize, h: usize) -> Tile {
        Tile { w, h, px: vec![0; w * h * 4] }
    }

    /// Source-over of premultiplied `src` onto self at (x0, y0), clipped.
    fn over(&mut self, src: &Tile, x0: i64, y0: i64) {
        blend(&mut self.px, self.w, self.h, src, x0, y0, true);
    }
}

/// Source-over blend of `src` into `dst`. For a frame (`premul_dst == false`)
/// the destination is straight RGBA and treated as opaque-ish; for a tile it is
/// premultiplied. Clipped to the destination rectangle.
fn blend(dst: &mut [u8], dw: usize, dh: usize, src: &Tile, x0: i64, y0: i64, premul_dst: bool) {
    for sy in 0..src.h {
        let y = y0 + sy as i64;
        if y < 0 || y >= dh as i64 {
            continue;
        }
        for sx in 0..src.w {
            let x = x0 + sx as i64;
            if x < 0 || x >= dw as i64 {
                continue;
            }
            let s = &src.px[(sy * src.w + sx) * 4..][..4];
            let sa = s[3] as u32;
            if sa == 0 {
                continue;
            }
            let i = (y as usize * dw + x as usize) * 4;
            let d = &mut dst[i..i + 4];
            let inv = 255 - sa;
            for c in 0..3 {
                d[c] = (s[c] as u32 + (d[c] as u32 * inv + 127) / 255).min(255) as u8;
            }
            d[3] = if premul_dst { (sa + (d[3] as u32 * inv + 127) / 255).min(255) as u8 } else { 255 };
        }
    }
}

fn box_blur_alpha(a: &mut [f32], w: usize, h: usize, r: usize) {
    let mut tmp = vec![0.0f32; a.len()];
    let n = (2 * r + 1) as f32;
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for k in x.saturating_sub(r)..=(x + r).min(w - 1) {
                s += a[y * w + k];
            }
            tmp[y * w + x] = s / n;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for k in y.saturating_sub(r)..=(y + r).min(h - 1) {
                s += tmp[k * w + x];
            }
            a[y * w + x] = s / n;
        }
    }
}

/// Put a soft black halo under `ink` (a tile that already has `HALO_PAD` of
/// empty border on every side) and return ink-over-halo.
fn with_halo(ink: &Tile) -> Tile {
    let mut a: Vec<f32> = ink.px.chunks_exact(4).map(|p| p[3] as f32 / 255.0).collect();
    box_blur_alpha(&mut a, ink.w, ink.h, HALO_RADIUS);
    box_blur_alpha(&mut a, ink.w, ink.h, HALO_RADIUS / 2);
    let mut out = Tile::new(ink.w, ink.h);
    for (i, v) in a.iter().enumerate() {
        out.px[i * 4 + 3] = ((v * 3.0).min(1.0) * 255.0).round() as u8;
    }
    out.over(ink, 0, 0);
    out
}

/// Rasterise a symbol in white so 1000 units = `cap_px`, with a `HALO_PAD`
/// empty border, then add the halo.
fn render_mark(id: &str, cap_px: f32) -> Result<Tile> {
    let inner = rasterise_symbol(MARK_SVG, id, cap_px, "white")?;
    let (iw, ih) = (inner.width() as usize, inner.height() as usize);
    let (w, h) = (iw + 2 * HALO_PAD, ih + 2 * HALO_PAD);
    let mut ink = Tile::new(w, h);
    for y in 0..ih {
        let dst = ((y + HALO_PAD) * w + HALO_PAD) * 4;
        ink.px[dst..dst + iw * 4].copy_from_slice(&inner.data()[y * iw * 4..(y + 1) * iw * 4]);
    }
    Ok(with_halo(&ink))
}

fn shape_line(fs: &mut FontSystem, family: &str, text: &str, size: f32, weight: Weight, max_w: f32) -> (Buffer, f32) {
    let attrs = Attrs::new().family(Family::Name(family)).weight(weight);
    // Titles come from untrusted tags: bound the input before the O(n) reshape loop.
    let mut chars: Vec<char> = text.chars().filter(|c| !c.is_control()).take(MAX_TEXT_CHARS).collect();
    let mut truncated = false;
    loop {
        let mut s: String = chars.iter().collect();
        if truncated {
            s.push('\u{2026}');
        }
        let mut buf = Buffer::new(fs, Metrics::new(size, size * 1.2));
        buf.set_size(None, None);
        buf.set_text(&s, &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(fs, false);
        let w = buf.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
        if w <= max_w || chars.is_empty() {
            return (buf, w);
        }
        chars.pop();
        truncated = true;
    }
}

fn draw_text_into(ink: &mut Tile, fs: &mut FontSystem, cache: &mut SwashCache, buf: &mut Buffer, x0: i64, y0: i64, grey: u8) {
    let col = cosmic_text::Color::rgb(grey, grey, grey);
    buf.draw(fs, cache, col, |x, y, w, h, c| {
        let a = c.a() as u32;
        if a == 0 {
            return;
        }
        for yy in y..y + h as i32 {
            for xx in x..x + w as i32 {
                let (px, py) = (x0 + xx as i64, y0 + yy as i64);
                if px < 0 || py < 0 || px >= ink.w as i64 || py >= ink.h as i64 {
                    continue;
                }
                let i = (py as usize * ink.w + px as usize) * 4;
                let inv = 255 - a;
                let v = (grey as u32 * a / 255) as u8;
                for k in 0..3 {
                    ink.px[i + k] = (v as u32 + ink.px[i + k] as u32 * inv / 255).min(255) as u8;
                }
                ink.px[i + 3] = (a + ink.px[i + 3] as u32 * inv / 255).min(255) as u8;
            }
        }
    });
}

/// Right-aligned title/artist text with halo. Err if no font is available:
/// a render must not complete and ship without its title.
fn render_text(title: &str, artist: Option<&str>) -> Result<Tile> {
    render_text_with(&mut production_font_system(), FONT_FAMILY, title, artist)
}

fn render_text_with(fs: &mut FontSystem, family: &str, title: &str, artist: Option<&str>) -> Result<Tile> {
    if fs.db().is_empty() {
        return Err(Error::msg("overlay: no fonts available to draw the title"));
    }
    let mut cache = SwashCache::new();
    let (mut tb, tw) = shape_line(fs, family, title, 34.0, Weight::BOLD, TEXT_MAX_W);
    let mut ab = artist.map(|a| shape_line(fs, family, a, 24.0, Weight::NORMAL, TEXT_MAX_W));
    let w = tw.max(ab.as_ref().map_or(0.0, |a| a.1)).ceil() as usize;
    let (th, ah) = (41, 29);
    let h = th + if ab.is_some() { ah } else { 0 };
    let mut ink = Tile::new(w + 2 * HALO_PAD, h + 2 * HALO_PAD);
    let p = HALO_PAD as i64;
    draw_text_into(&mut ink, fs, &mut cache, &mut tb, p + (w as f32 - tw).round() as i64, p, 255);
    if let Some((buf, aw)) = ab.as_mut() {
        let aw = *aw;
        draw_text_into(&mut ink, fs, &mut cache, buf, p + (w as f32 - aw).round() as i64, p + th as i64, 215);
    }
    Ok(with_halo(&ink))
}

/// Decode, centre-crop to square and resize to `ART_PX`. Bounded: file size,
/// decoder allocation and dimensions are capped; any failure is an Err.
fn load_art(path: &Path) -> Result<Tile> {
    let len = std::fs::metadata(path).map_err(|e| Error::msg(format!("stat: {e}")))?.len();
    if len > MAX_ART_FILE_BYTES {
        return Err(Error::msg(format!("art file is {len} bytes")));
    }
    let io = |e: std::io::Error| Error::msg(format!("read: {e}"));
    let mut reader = image::ImageReader::open(path).map_err(io)?.with_guessed_format().map_err(io)?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_ART_DECODE_BYTES);
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| Error::msg(format!("decode: {e}")))?.to_rgba8();
    let (w, h) = img.dimensions();
    let side = w.min(h);
    if side == 0 {
        return Err(Error::msg("empty image"));
    }
    let crop = image::imageops::crop_imm(&img, (w - side) / 2, (h - side) / 2, side, side).to_image();
    let small = image::imageops::resize(&crop, ART_PX, ART_PX, image::imageops::FilterType::Triangle);
    let mut t = Tile::new(ART_PX as usize, ART_PX as usize);
    for (i, p) in small.pixels().enumerate() {
        let a = p[3] as u32;
        for c in 0..3 {
            t.px[i * 4 + c] = (p[c] as u32 * a / 255) as u8;
        }
        t.px[i * 4 + 3] = a as u8;
    }
    Ok(t)
}

/// Black scrim whose alpha ramps up over `SCRIM_FEATHER` px from every edge.
fn scrim(w: usize, h: usize) -> Tile {
    let mut t = Tile::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let d = x.min(w - 1 - x).min(y).min(h - 1 - y) as f32;
            let k = (d / SCRIM_FEATHER).clamp(0.0, 1.0);
            let k = k * k * (3.0 - 2.0 * k);
            t.px[(y * w + x) * 4 + 3] = (SCRIM_ALPHA * k * 255.0).round() as u8;
        }
    }
    t
}

///
/// Geometry (margins, art size, text size, watermark size) is fixed in pixels
/// for 1080p frames. Other frame sizes are safe (everything is clipped to the
/// buffer) but are not rescaled.
pub struct Overlay {
    /// Scrim + text + art, one tile, anchored top-right.
    block: Option<Tile>,
    /// Watermark tiles, index 0 = scale 1.0, last = `MAX_MARK_SCALE`.
    marks: Vec<Tile>,
    reactive: bool,
}

impl Overlay {
    pub fn new(title: &str, artist: Option<&str>, art: Option<&Path>, style: MarkStyle, reactive: bool) -> Result<Overlay> {
        let art_tile = art.and_then(|p| match load_art(p) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("overlay: cover art {} skipped: {}", p.display(), e.0);
                None
            }
        });
        let text = if title.is_empty() && artist.is_none() { None } else { Some(render_text(title, artist)?) };

        let block = if text.is_none() && art_tile.is_none() {
            None
        } else {
            let (tw, th) = text.as_ref().map_or((0, 0), |t| (t.w, t.h));
            let content_h = th.max(if art_tile.is_some() { ART_PX as usize } else { 0 });
            let content_w = tw
                + if art_tile.is_some() { ART_PX as usize + if text.is_some() { BLOCK_GAP } else { 0 } } else { 0 };
            let mut b = scrim(content_w + 2 * BLOCK_PAD, content_h + 2 * BLOCK_PAD);
            if let Some(t) = &text {
                b.over(t, BLOCK_PAD as i64, (BLOCK_PAD + (content_h - t.h) / 2) as i64);
            }
            if let Some(a) = &art_tile {
                b.over(a, (BLOCK_PAD + content_w - ART_PX as usize) as i64, (BLOCK_PAD + (content_h - a.h) / 2) as i64);
            }
            Some(b)
        };

        let levels = if reactive { SCALE_LEVELS } else { 1 };
        let marks = (0..levels)
            .map(|i| {
                let s = 1.0 + (MAX_MARK_SCALE - 1.0) * i as f32 / (SCALE_LEVELS - 1) as f32;
                render_mark(style.symbol(), style.cap_px() * s)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Overlay { block, marks, reactive })
    }

    /// Blend the overlay into `frame` (`w`*`h` RGBA8, tightly packed). `bass` is
    /// clamped to 0..=1 and ignored unless the overlay is reactive. A buffer
    /// shorter than `w*h*4` is left untouched.
    pub fn draw(&self, frame: &mut [u8], w: u32, h: u32, bass: f32) {
        let (w, h) = (w as usize, h as usize);
        match w.checked_mul(h).and_then(|n| n.checked_mul(4)) {
            Some(need) if need > 0 && frame.len() >= need => {}
            _ => return,
        }
        let m = SAFE_MARGIN as i64;
        if let Some(b) = &self.block {
            blend(frame, w, h, b, w as i64 - m - b.w as i64, m, false);
        }
        let bass = if self.reactive && !bass.is_nan() { bass.clamp(0.0, 1.0) } else { 0.0 };
        let idx = (bass * (self.marks.len() - 1) as f32).round() as usize;
        let t = &self.marks[idx];
        // Anchored at the padded tile's lower-left corner: growth goes up and right.
        blend(frame, w, h, t, m, h as i64 - m - t.h as i64, false);
    }
}
