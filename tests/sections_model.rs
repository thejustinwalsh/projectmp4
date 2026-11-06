#[path = "common/agentdeck.rs"]
mod agentdeck;
use projectmp4::audio::sections::model::MAX_WORKING_BYTES;
use projectmp4::audio::sections::*;
use std::path::{Path, PathBuf};

const MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/models/harmonix-fold0-f32.gguf");

fn weights() -> Weights {
    Weights::load(Path::new(MODEL)).unwrap()
}

fn json(p: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(p).unwrap_or_else(|e| panic!("{p:?}: {e}"))).unwrap()
}

struct Pair {
    id: String,
    frames: usize,
    tensor: PathBuf,
    segments: Vec<serde_json::Value>,
}

/// Every cached result paired with its cached spectrogram, by the recorded input identity.
fn pairs() -> Vec<Pair> {
    let root = agentdeck::root();
    if !root.exists() {
        if std::env::var_os("PROJECTMP4_NO_PARITY_ORACLE").is_some() {
            eprintln!("SKIPPED by PROJECTMP4_NO_PARITY_ORACLE: {} absent", root.display());
            return Vec::new();
        }
        panic!(
            "parity oracle missing: {} does not exist.\n\
             This gate is the only check that the forward pass matches the reference, so it must \
             not pass by default when it cannot run. Set PROJECTMP4_NO_PARITY_ORACLE=1 to opt out.",
            root.display()
        );
    }
    let art = root.join("sections/jobs/artifacts");
    let mut out = Vec::new();
    let mut names: Vec<_> = std::fs::read_dir(&art).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    for n in names {
        let Some(id) = n.strip_suffix(".json") else { continue };
        if id.ends_with(".stage") {
            continue;
        }
        // A tensor's own metadata sits beside binary data; only results parse as JSON.
        let Ok(result) = serde_json::from_slice::<serde_json::Value>(&std::fs::read(art.join(id)).unwrap()) else { continue };
        let Some(r) = result.get("result") else { continue };
        let meta = json(&art.join(&n));
        let tensor = art.join(meta["inputs"][0]["identity"].as_str().unwrap());
        let frames = r["frames"].as_u64().unwrap() as usize;
        assert_eq!(std::fs::metadata(&tensor).unwrap().len() as usize, frames * 4 * 81 * 4, "{id}: tensor size");
        out.push(Pair { id: id.to_string(), frames, tensor, segments: r["segments"].as_array().unwrap().clone() });
    }
    out
}

struct Compared {
    frames: usize,
    worst_boundary: f64,
    worst_confidence: f64,
    seconds: f64,
}

fn compare(w: &Weights, p: &Pair) -> Compared {
    let spec = agentdeck::read_f32(&p.tensor);
    let t0 = std::time::Instant::now();
    let logits = predict(w, &spec, p.frames).unwrap();
    let seconds = t0.elapsed().as_secs_f64();
    let mine = postprocess(&logits, p.frames).unwrap();
    assert_eq!(mine.len(), p.segments.len(), "{}: segment count", p.id);
    let (mut wb, mut wc) = (0f64, 0f64);
    for (i, (m, c)) in mine.iter().zip(&p.segments).enumerate() {
        assert_eq!(LABELS[m.label as usize], c["label"].as_str().unwrap(), "{} segment {i}: label", p.id);
        let (s, e) = (c["startSeconds"].as_f64().unwrap(), c["endSeconds"].as_f64().unwrap());
        wb = wb.max((m.start - s).abs()).max((m.end - e).abs());
        wc = wc.max((m.confidence as f64 - c["meanClassProbability"].as_f64().unwrap()).abs());
    }
    assert!(wb <= 0.02, "{}: worst boundary error {wb} s", p.id);
    assert!(wc <= 0.01, "{}: worst confidence error {wc}", p.id);
    Compared { frames: p.frames, worst_boundary: wb, worst_confidence: wc, seconds }
}

#[test]
fn segments_match_agent_decks_cached_result_from_the_cached_spectrogram() {
    let ps = pairs();
    if ps.is_empty() {
        return; // only reachable with the explicit opt-out
    }
    let w = weights();
    // Largest first so the memory ceiling is exercised early; a few tracks at a time.
    let mut order: Vec<&Pair> = ps.iter().collect();
    order.sort_by_key(|p| std::cmp::Reverse(p.frames));
    let queue = std::sync::Mutex::new(order);
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| loop {
                let Some(p) = queue.lock().unwrap().pop() else { break };
                let c = compare(&w, p);
                results.lock().unwrap().push(c);
            });
        }
    });
    let results = results.into_inner().unwrap();
    assert_eq!(results.len(), ps.len(), "every pair compared");
    assert!(results.len() >= 3 && results.iter().any(|c| c.frames <= 1200) && results.iter().any(|c| c.frames == 32880));
    let wb = results.iter().map(|c| c.worst_boundary).fold(0.0, f64::max);
    let wc = results.iter().map(|c| c.worst_confidence).fold(0.0, f64::max);
    eprintln!("compared {} pairs; worst boundary error {wb} s; worst confidence error {wc}", results.len());
    for c in &results {
        eprintln!("  {:>6} frames  {:6.1}s  boundary {:.4}  confidence {:.2e}", c.frames, c.seconds, c.worst_boundary, c.worst_confidence);
    }
}

#[test]
fn working_bytes_are_refused_before_they_are_allocated() {
    let frames = 60 * 60 * 100;
    let need = estimated_working_bytes(frames);
    assert_eq!(need, (16 << 20) + frames as u64 * 24576);
    assert!(need > MAX_WORKING_BYTES);
    // An empty spectrogram would fail the length check; the refusal must come first,
    // so no multi-gigabyte input or working set is ever built.
    let err = predict(&weights(), &[], frames).unwrap_err().to_string();
    assert!(err.contains(&need.to_string()), "{err}");
    assert_eq!(estimated_working_bytes(1), (16 << 20) + 10240 * 24576);
    assert!(estimated_working_bytes(32880) < MAX_WORKING_BYTES);
}

#[test]
fn degenerate_inputs_are_clean_errors() {
    let w = weights();
    assert!(predict(&w, &[], 0).is_err());
    assert!(predict(&w, &vec![0.0; 4 * 10 * 81 - 1], 10).is_err());
    assert!(predict(&w, &vec![0.0; 4 * 10 * 81 + 1], 10).is_err());
    let mut nan = vec![0.5f32; 4 * 10 * 81];
    nan[17] = f32::NAN;
    assert!(predict(&w, &nan, 10).unwrap_err().to_string().contains("non-finite"));
    nan[17] = f32::INFINITY;
    assert!(predict(&w, &nan, 10).is_err());
    assert!(postprocess(&[], 0).is_err());
    assert!(postprocess(&[0.0; 10], 1).is_err());
    assert!(postprocess(&[f32::NAN; 11], 1).is_err());
}

#[test]
fn a_tiny_clip_runs_and_is_finite() {
    // 1 frame: every window is fully inside padding. Must not panic on clamp bounds.
    let w = weights();
    let l = predict(&w, &vec![0.3; 4 * 81], 1).unwrap();
    assert_eq!(l.len(), 11);
    let s = postprocess(&l, 1).unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!((s[0].start, s[0].end), (0.0, 0.01));
}

#[test]
fn segments_tile_the_track_in_order() {
    let ps = pairs();
    let Some(p) = ps.iter().min_by_key(|p| p.frames) else { return };
    let w = weights();
    let logits = predict(&w, &agentdeck::read_f32(&p.tensor), p.frames).unwrap();
    let s = postprocess(&logits, p.frames).unwrap();
    assert_eq!(s[0].start, 0.0);
    for pair in s.windows(2) {
        assert_eq!(pair[0].end, pair[1].start);
        assert!(pair[0].start < pair[0].end);
    }
    assert!((s.last().unwrap().end - p.frames as f64 / 100.0).abs() <= 0.01);
}
