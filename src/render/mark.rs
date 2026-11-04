//! SVG symbol rasterisation shared by the overlay and the mark tests/examples.
use crate::ffi::{Error, Result};
use resvg::tiny_skia::{Pixmap, Transform};

/// `viewBox` ([x, y, w, h]) of `<symbol id="...">`.
pub fn symbol_viewbox(svg: &str, id: &str) -> Result<[f64; 4]> {
    let start = svg
        .find(&format!("<symbol id=\"{id}\""))
        .ok_or_else(|| Error::msg(format!("symbol id {id:?} does not exist in the SVG")))?;
    let end = svg[start..].find('>').ok_or_else(|| Error::msg("unterminated symbol tag"))? + start;
    let vb = svg[start..end]
        .split("viewBox=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .ok_or_else(|| Error::msg(format!("symbol {id:?} has no viewBox")))?;
    let n: Vec<f64> = vb.split_whitespace().filter_map(|v| v.parse().ok()).collect();
    if n.len() != 4 {
        return Err(Error::msg(format!("bad viewBox {vb:?}")));
    }
    Ok([n[0], n[1], n[2], n[3]])
}

/// Rasterise `<symbol id>` so 1000 viewBox units (the cap height) span `cap_px`
/// pixels. The canvas follows the symbol's own viewBox aspect; `ink` is the CSS
/// colour substituted for `currentColor`.
pub fn rasterise_symbol(svg: &str, id: &str, cap_px: f32, ink: &str) -> Result<Pixmap> {
    let [vx, vy, vw, vh] = symbol_viewbox(svg, id)?;
    let k = cap_px as f64 / 1000.0;
    let (w, h) = ((vw * k).round() as u32, (vh * k).round() as u32);
    if !svg.starts_with("<svg") {
        return Err(Error::msg("unexpected svg root"));
    }
    let root_end = svg.find('>').ok_or_else(|| Error::msg("bad svg"))?;
    let body_end = svg.rfind("</svg>").ok_or_else(|| Error::msg("bad svg"))?;
    let doc = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{vw}\" height=\"{vh}\" \
         viewBox=\"{vx} {vy} {vw} {vh}\" color=\"{ink}\">{}<use href=\"#{id}\" x=\"{vx}\" y=\"{vy}\" \
         width=\"{vw}\" height=\"{vh}\"/></svg>",
        &svg[root_end + 1..body_end]
    );
    let tree = resvg::usvg::Tree::from_str(&doc, &resvg::usvg::Options::default())
        .map_err(|e| Error::msg(format!("parse mark svg: {e}")))?;
    let mut pixmap = Pixmap::new(w, h).ok_or_else(|| Error::msg(format!("symbol {id:?} rasterises to an empty canvas")))?;
    let t = Transform::from_scale(w as f32 / vw as f32, h as f32 / vh as f32);
    resvg::render(&tree, t, &mut pixmap.as_mut());
    Ok(pixmap)
}
