#[path = "common/mark.rs"]
mod mark;
use mark::*;
use std::collections::BTreeSet;
use std::fs;

fn distinct_fill_colours(svg: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for attr in ["fill=\"", "stroke=\""] {
        for part in svg.split(attr).skip(1) {
            let v = part.split('"').next().unwrap();
            if v != "none" {
                out.insert(v.to_string());
            }
        }
    }
    out
}

#[test]
fn mark_is_single_colour() {
    let svg = fs::read_to_string(MARK_SVG).unwrap();
    assert!(!svg.contains("linearGradient") && !svg.contains("radialGradient"));
    let colours = distinct_fill_colours(&svg);
    assert_eq!(colours.len(), 1, "{colours:?}");
    assert!(colours.contains("currentColor"), "{colours:?}");
}

#[test]
fn mark_renders_legibly_at_watermark_size() {
    let svg = fs::read_to_string(MARK_SVG).unwrap();
    let mut sigs = Vec::new();
    for id in ["word", "sigil"] {
        let r = render_svg_symbol(MARK_SVG, id, 28, "white");
        assert!(r.px.iter().any(|p| p[3] > 0), "{id} rendered empty");

        let cov = ink_coverage(&r.px);
        assert!(cov > 0.04 && cov < 0.60, "{id} coverage {cov:.3} at 28px: too sparse or a solid block");

        // Canvas aspect must follow the symbol's own viewBox (rounding allows a few %).
        let [_, _, vw, vh] = symbol_viewbox(&svg, id);
        let (got, want) = (r.w as f64 / r.h as f64, vw / vh);
        assert!((got / want - 1.0).abs() < 0.05, "{id} aspect {got:.3} vs viewBox {want:.3}");

        // Ink spreads across the canvas rather than sitting in one corner.
        let (bw, bh) = ink_bbox_fraction(&r);
        assert!(bw > 0.75 && bh > 0.75, "{id} ink bbox only {bw:.2} x {bh:.2} of canvas");

        sigs.push(shape_signature(&r));
    }

    // The two symbols must be different drawings, and the metric must see
    // identical drawings as identical (so it can fail in both directions).
    let again = shape_signature(&render_svg_symbol(MARK_SVG, "word", 28, "white"));
    assert!(signature_distance(&sigs[0], &again) < 1e-9, "render not deterministic");
    let d = signature_distance(&sigs[0], &sigs[1]);
    assert!(d > 0.05, "word and sigil look the same (distance {d:.4})");
}
