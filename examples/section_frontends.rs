//! Measures how well the section model does on four channels derived from the mix alone,
//! against labels computed from real htdemucs stems. Reads agent-deck's cache (read-only).
//!
//!   cargo run --release --example section_frontends -- [out.md]
//!
//! Not part of `cargo test`: it runs the model 3x per track. The control row (CachedStems)
//! must score 1.00; if it does not, a component below the front end regressed.
#[path = "../tests/common/agentdeck.rs"]
mod agentdeck;
#[path = "../tests/common/cached_stems.rs"]
mod cached_stems;

use projectmp4::audio::sections::frontend::*;
use projectmp4::audio::sections::spectrogram::{frame_bands, frames_for, resample_48k_to_44k1, stack, HOP};
use projectmp4::audio::sections::*;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MODEL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/models/harmonix-fold0-f32.gguf");

#[derive(Clone, Debug)]
struct Seg {
    start: f64,
    end: f64,
    label: String,
    conf: f64,
}

struct Pair {
    id: String,
    frames: usize,
    bundle: PathBuf,
    reference: Vec<Seg>,
    /// Downbeat times (s) from agent-deck's cached beat grid for the same source; empty if absent.
    downbeats: Vec<f32>,
}

fn pairs() -> Vec<Pair> {
    let root = agentdeck::root();
    assert!(root.exists(), "{} missing", root.display());
    // bundleHash (inside each manifest) -> directory; the directory name is not the hash.
    let mut bundles = std::collections::HashMap::new();
    for e in std::fs::read_dir(root.join("stems/bundles")).unwrap() {
        let d = e.unwrap().path();
        let Ok(b) = std::fs::read(d.join("manifest.json")) else { continue };
        let Ok(m) = serde_json::from_slice::<serde_json::Value>(&b) else { continue };
        if let Some(h) = m["bundleHash"].as_str() {
            if ["bass", "drums", "other", "vocals"].iter().all(|n| d.join(format!("{n}.wav")).exists()) {
                bundles.insert(h.to_string(), d);
            }
        }
    }
    let art = root.join("sections/jobs/artifacts");
    let mut names: Vec<_> = std::fs::read_dir(&art).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    let mut out = Vec::new();
    for n in names {
        let Some(id) = n.strip_suffix(".json") else { continue };
        if id.ends_with(".stage") {
            continue;
        }
        let Ok(result) = serde_json::from_slice::<serde_json::Value>(&std::fs::read(art.join(id)).unwrap()) else { continue };
        let Some(r) = result.get("result") else { continue };
        let Some(bundle) = result["stemBundleHash"].as_str().and_then(|h| bundles.get(h)) else {
            eprintln!("{id}: stem bundle not present, skipped");
            continue;
        };
        let reference = r["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| Seg {
                start: c["startSeconds"].as_f64().unwrap(),
                end: c["endSeconds"].as_f64().unwrap(),
                label: c["label"].as_str().unwrap().to_string(),
                conf: c["meanClassProbability"].as_f64().unwrap(),
            })
            .collect();
        let downbeats = result["sourceSha256"]
            .as_str()
            .and_then(|h| std::fs::read(root.join(format!("beats/{h}.json"))).ok())
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v["grid"]["downbeats"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect()))
            .unwrap_or_default();
        out.push(Pair { id: id.to_string(), frames: r["frames"].as_u64().unwrap() as usize, bundle: bundle.clone(), reference, downbeats });
    }
    out
}

fn to_segs(s: &[Section]) -> Vec<Seg> {
    s.iter().map(|x| Seg { start: x.start, end: x.end, label: LABELS[x.label as usize].to_string(), conf: x.confidence as f64 }).collect()
}

/// Interior boundaries: starts of real segments after the first, ignoring the 10 ms
/// `start`/`end` markers.
fn boundaries(s: &[Seg]) -> Vec<f64> {
    let real: Vec<&Seg> = s.iter().filter(|g| g.label != "start" && g.label != "end").collect();
    real.iter().skip(1).map(|g| g.start).collect()
}

/// F-measure with one-to-one matching inside +-tol (greedy on sorted lists is optimal in 1-D).
fn boundary_f(est: &[f64], reference: &[f64], tol: f64) -> f64 {
    if est.is_empty() && reference.is_empty() {
        return 1.0;
    }
    if est.is_empty() || reference.is_empty() {
        return 0.0;
    }
    let (mut i, mut j, mut m) = (0, 0, 0usize);
    while i < est.len() && j < reference.len() {
        if (est[i] - reference[j]).abs() <= tol {
            m += 1;
            i += 1;
            j += 1;
        } else if est[i] < reference[j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    if m == 0 {
        return 0.0;
    }
    let (p, r) = (m as f64 / est.len() as f64, m as f64 / reference.len() as f64);
    2.0 * p * r / (p + r)
}

fn label_at(s: &[Seg], t: f64) -> &str {
    s.iter().find(|g| t >= g.start && t < g.end).or(s.last()).map(|g| g.label.as_str()).unwrap_or("")
}

/// Fraction of 10 ms frames whose label agrees.
fn label_accuracy(est: &[Seg], reference: &[Seg], frames: usize) -> f64 {
    let hit = (0..frames)
        .filter(|f| {
            let t = *f as f64 / 100.0 + 0.005;
            label_at(est, t) == label_at(reference, t)
        })
        .count();
    hit as f64 / frames as f64
}

fn best_chorus(s: &[Seg]) -> Option<&Seg> {
    let mut best: Option<&Seg> = None;
    for g in s.iter().filter(|g| g.label == "chorus") {
        if best.map_or(true, |b| g.conf > b.conf) {
            best = Some(g);
        }
    }
    best
}

#[derive(Clone, Copy, Default)]
struct Clip {
    /// False: the reference has no chorus, so the track is outside the metric.
    applicable: bool,
    /// overlap >= 0.5 x the chosen span.
    chosen: bool,
    /// overlap >= 0.5 x the longer of the two spans.
    longer: bool,
}

fn clip(est: &[Seg], reference: &[Seg]) -> Clip {
    let Some(r) = best_chorus(reference) else { return Clip::default() };
    let Some(e) = best_chorus(est) else { return Clip { applicable: true, chosen: false, longer: false } };
    let ov = (e.end.min(r.end) - e.start.max(r.start)).max(0.0);
    let (de, dr) = (e.end - e.start, r.end - r.start);
    Clip { applicable: true, chosen: ov >= 0.5 * de, longer: ov >= 0.5 * de.max(dr) }
}

#[derive(Clone)]
struct Score {
    f05: f64,
    f3: f64,
    acc: f64,
    clip: Clip,
    sections: usize,
}

fn run(w: &Weights, fe: &dyn FourChannel, mix: &[f32]) -> projectmp4::ffi::Result<Vec<Section>> {
    let ch = fe.channels(mix, 48_000)?;
    let r = |i: usize| resample_48k_to_44k1(&ch[i]);
    let stems = [r(0)?, r(1)?, r(2)?, r(3)?];
    drop(ch);
    let (spec, frames) = stack(&stems)?;
    drop(stems);
    postprocess(&predict(w, &spec, frames)?, frames)
}

/// The 2048-sample window the spectrogram uses for frame `t`: centred, zero outside the signal.
fn window_at(pcm: &[f32], t: usize) -> [f32; 2048] {
    let mut w = [0f32; 2048];
    let start = (t * HOP) as i64 - 1024;
    for (j, v) in w.iter_mut().enumerate() {
        let i = start + j as i64;
        if i >= 0 && (i as usize) < pcm.len() {
            *v = pcm[i as usize];
        }
    }
    w
}

/// Per-frame band energies (81 bands) for a 48 kHz mono signal.
fn band_frames(mono_48k: &[f32]) -> projectmp4::ffi::Result<Vec<[f32; 81]>> {
    let pcm = resample_48k_to_44k1(mono_48k)?;
    (0..frames_for(pcm.len()))
        .map(|t| {
            let mut b = [0f32; 81];
            frame_bands(&window_at(&pcm, t), &mut b)?;
            Ok(b)
        })
        .collect()
}

/// Window of `span` frames with the most energy in bands `0..cut`, earliest on ties, then
/// snapped to the nearest downbeat if the snapped span still fits.
fn pick_peak(bands: &[[f32; 81]], cut: usize, span: usize, downbeats_s: &[f32]) -> (usize, usize) {
    let frames = bands.len();
    let mut prefix = vec![0f64; frames + 1];
    for (t, b) in bands.iter().enumerate() {
        prefix[t + 1] = prefix[t] + b[..cut].iter().sum::<f32>() as f64;
    }
    let (mut start, mut best) = (0usize, f64::NEG_INFINITY);
    for s in 0..=frames.saturating_sub(span) {
        let e = prefix[(s + span).min(frames)] - prefix[s];
        if e > best {
            best = e;
            start = s;
        }
    }
    let snapped = downbeats_s
        .iter()
        .map(|s| (s * 100.0).round() as i64)
        .min_by_key(|f| (f - start as i64).abs())
        .filter(|f| *f >= 0 && *f as usize + span <= frames);
    if let Some(f) = snapped {
        start = f as usize;
    }
    (start, start + span)
}

/// Task 20b picker: low-band (bands 0..24 of 81) energy peak, no model.
fn energy_peak_clip(mono_48k: &[f32], span_frames: usize, downbeats_s: &[f32]) -> (usize, usize) {
    pick_peak(&band_frames(mono_48k).unwrap(), 24, span_frames, downbeats_s)
}

const CUTS: [usize; 3] = [16, 24, 32];

/// Scores a picked span with the harness's own `clip` predicate by presenting it as the
/// estimate's one chorus.
fn score_pick(span: (usize, usize), reference: &[Seg]) -> Clip {
    let est = [Seg { start: span.0 as f64 / 100.0, end: span.1 as f64 / 100.0, label: "chorus".into(), conf: 1.0 }];
    clip(&est, reference)
}

const NAMES: [&str; 3] = ["CachedStems (htdemucs)", "HpssBands", "MixFourWays"];

/// (clip outcome, picked start frame, reference start frame, span) per band cut.
type Pick = (Clip, usize, usize, usize);
type Row = (String, usize, [Result<Score, String>; 3], [Option<Pick>; 3]);

fn main() {
    let out_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".superpowers/sdd/2026-11-06-standalone-sections/frontend-measurement.md".into());
    let w = Weights::load(Path::new(MODEL)).unwrap();
    let mut ps = pairs();
    ps.sort_by_key(|p| std::cmp::Reverse(p.frames));
    let n_tracks = ps.len();
    let queue = Mutex::new(ps.iter().collect::<Vec<_>>());
    let results: Mutex<Vec<Row>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..3 {
            s.spawn(|| loop {
                let Some(p) = queue.lock().unwrap().pop() else { break };
                let t0 = std::time::Instant::now();
                let cached = cached_stems::CachedStems::new(&p.bundle);
                // The "mix": the sum of the four stems (what htdemucs partitions), mono, 48 kHz.
                let len = agentdeck::read_wav48_mono(&p.bundle.join("drums.wav")).len();
                let st = cached.channels(&vec![0.0; len], 48_000).unwrap();
                let mix: Vec<f32> = (0..len).map(|i| st[0][i] + st[1][i] + st[2][i] + st[3][i]).collect();
                drop(st);
                let mut band_frames_cache: Option<Vec<[f32; 81]>> = None;
                let fes: [&dyn FourChannel; 3] = [&cached, &HpssBands, &MixFourWays];
                let ref_b = boundaries(&p.reference);
                let res: [Result<Score, String>; 3] = std::array::from_fn(|i| {
                    let secs = run(&w, fes[i], &mix).map_err(|e| e.0.to_string())?;
                    let segs = to_segs(&secs);
                    let b = boundaries(&segs);
                    Ok(Score {
                        f05: boundary_f(&b, &ref_b, 0.5),
                        f3: boundary_f(&b, &ref_b, 3.0),
                        acc: label_accuracy(&segs, &p.reference, p.frames),
                        clip: clip(&segs, &p.reference),
                        sections: segs.len(),
                    })
                });
                let picks: [Option<Pick>; 3] = std::array::from_fn(|i| {
                    let r = best_chorus(&p.reference)?;
                    let span = ((r.end - r.start) * 100.0).round() as usize;
                    let bands = band_frames_cache.get_or_insert_with(|| band_frames(&mix).unwrap());
                    let sp = pick_peak(bands, CUTS[i], span, &p.downbeats);
                    if CUTS[i] == 24 {
                        assert_eq!(sp, energy_peak_clip(&mix, span, &p.downbeats), "cached-band path must equal the spec'd picker");
                    }
                    Some((score_pick(sp, &p.reference), sp.0, (r.start * 100.0).round() as usize, span))
                });
                eprintln!("{} ({} frames) done in {:.0}s", &p.id[..8], p.frames, t0.elapsed().as_secs_f64());
                results.lock().unwrap().push((p.id.clone(), p.frames, res, picks));
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|r| r.0.clone());

    let mut md = String::new();
    md += &format!(
        "# Section model: stem-free front ends vs htdemucs stems\n\n{n_tracks} tracks (every cached result whose stem bundle is present). Reference = agent-deck's cached labels from real htdemucs stems.\nFront-end input = mono sum of the four stems at 48 kHz (what htdemucs partitions).\n\n"
    );
    md += "| front end | boundary F @0.5s | boundary F @3s | label accuracy (frame-weighted) | chosen-clip agreement (>=50% of chosen span) | chosen-clip agreement (>=50% of longer span) | tracks failed |\n|---|---|---|---|---|---|---|\n";
    let mut failed_notes = Vec::new();
    for k in 0..3 {
        let ok: Vec<&Score> = results.iter().filter_map(|r| r.2[k].as_ref().ok()).collect();
        for r in &results {
            if let Err(e) = &r.2[k] {
                failed_notes.push(format!("{} on {}: {e}", NAMES[k], r.0));
            }
        }
        let mean = |f: &dyn Fn(&Score) -> f64| ok.iter().map(|s| f(s)).sum::<f64>() / ok.len().max(1) as f64;
        let app: Vec<&&Score> = ok.iter().filter(|s| s.clip.applicable).collect();
        let c1 = app.iter().filter(|s| s.clip.chosen).count();
        let c2 = app.iter().filter(|s| s.clip.longer).count();
        let d = app.len().max(1) as f64;
        md += &format!(
            "| {} | {:.3} | {:.3} | {:.4} | {:.3} ({}/{}) | {:.3} ({}/{}) | {} |\n",
            NAMES[k],
            mean(&|s| s.f05),
            mean(&|s| s.f3),
            mean(&|s| s.acc),
            c1 as f64 / d,
            c1,
            app.len(),
            c2 as f64 / d,
            c2,
            app.len(),
            results.len() - ok.len()
        );
    }
    {
        let line = |k: usize| {
            let app: Vec<&Pick> = results.iter().filter_map(|r| r.3[k].as_ref()).collect();
            let c = app.iter().filter(|p| p.0.chosen).count();
            let l = app.iter().filter(|p| p.0.longer).count();
            let d = app.len().max(1) as f64;
            (c, l, app.len(), c as f64 / d, l as f64 / d)
        };
        let (c, l, n, cf, lf) = line(1);
        md += &format!("| EnergyPeak (low bands 0..24, no model) | n/a | n/a | n/a | {cf:.3} ({c}/{n}) | {lf:.3} ({l}/{n}) | 0 |\n");
    }
    md += "\nChosen-clip agreement is over tracks whose reference has at least one `chorus`; a front end that finds no chorus there scores a miss. Tracks without a reference chorus are outside the metric.\n";
    md += &format!(
        "Reference tracks with no chorus: {}.\n",
        results.iter().filter(|r| r.2[0].as_ref().map_or(false, |s| !s.clip.applicable)).count()
    );
    if failed_notes.is_empty() {
        md += "\nNo front end failed outright on any track.\n";
    } else {
        md += "\nFailures:\n";
        for f in &failed_notes {
            md += &format!("- {f}\n");
        }
    }
    md += "\n## EnergyPeak band-cut sensitivity (same tracks, same spans, downbeat-snapped)\n\n| low-band cut | chosen-clip agreement | longer-span agreement |\n|---|---|---|\n";
    for k in 0..3 {
        let app: Vec<&Pick> = results.iter().filter_map(|r| r.3[k].as_ref()).collect();
        let c = app.iter().filter(|p| p.0.chosen).count();
        let l = app.iter().filter(|p| p.0.longer).count();
        md += &format!("| {} of 81 | {:.3} ({}/{}) | {:.3} ({}/{}) |\n", CUTS[k], c as f64 / app.len() as f64, c, app.len(), l as f64 / app.len() as f64, l, app.len());
    }
    md += "\n## Per track (F@0.5 / F@3 / label acc / chosen-clip, sections)\n\n| track | frames | CachedStems | HpssBands | MixFourWays | EnergyPeak 16 / 24 / 32 (picked start vs ref start, s) |\n|---|---|---|---|---|---|\n";
    for (id, frames, r, picks) in &results {
        let cell = |x: &Result<Score, String>| match x {
            Ok(s) => format!(
                "{:.2} / {:.2} / {:.2} / {}, {}",
                s.f05,
                s.f3,
                s.acc,
                if !s.clip.applicable { "n/a" } else if s.clip.chosen { "hit" } else { "miss" },
                s.sections
            ),
            Err(e) => format!("FAILED: {e}"),
        };
        let pc: Vec<String> = picks
            .iter()
            .map(|p| match p {
                None => "n/a".to_string(),
                Some((c, s, rs, _)) => format!("{} ({:.1} vs {:.1})", if c.chosen { "hit" } else { "miss" }, *s as f64 / 100.0, *rs as f64 / 100.0),
            })
            .collect();
        md += &format!("| {} | {} | {} | {} | {} | {} |\n", &id[..8], frames, cell(&r[0]), cell(&r[1]), cell(&r[2]), pc.join(" / "));
    }
    print!("{md}");
    std::fs::write(&out_path, &md).unwrap();
}
