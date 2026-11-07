//! htdemucs stem separation by driving the vendored `deck_stem_worker`
//! subprocess, with agent-deck's weighted overlap-add done here. A port of
//! `spanWeight` (`artifacts.ts`) and `stitch` (`producer.ts`).
//!
//! Output is 48 kHz stereo `f32`; no resampling happens here.

use crate::ffi::{Error, Result};
use sha2::{Digest, Sha256};
use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const STEM_SPAN_FRAMES: usize = 288_000;
pub const STEM_HOP_FRAMES: usize = 216_000;
pub const OVERLAP: usize = STEM_SPAN_FRAMES - STEM_HOP_FRAMES;
const MAX_SOURCE_FRAMES: usize = 48_000 * 600;
const ROLES: [&str; 4] = ["bass", "drums", "other", "vocals"];

/// 48 kHz stereo f32, interleaved, one entry per role.
pub struct Stems {
    pub bass: Vec<f32>,
    pub drums: Vec<f32>,
    pub other: Vec<f32>,
    pub vocals: Vec<f32>,
    pub frames: usize,
}

/// A resolved and hash-verified runtime package.
pub struct Runtime {
    worker: PathBuf,
    model: PathBuf,
}

fn sha256_hex(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .map_err(|e| Error::msg(format!("stems runtime: cannot read {}: {e}", path.display())))?;
    Ok(Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect())
}

fn verify(dir: &Path, rel: &str, want: &str) -> Result<PathBuf> {
    if rel.is_empty() || rel.contains('/') || rel.contains("..") {
        return Err(Error::msg(format!("stems runtime: {rel:?} is not a bare filename")));
    }
    let p = dir.join(rel);
    let got = sha256_hex(&p)?;
    if !got.eq_ignore_ascii_case(want) {
        return Err(Error::msg(format!(
            "stems runtime: {} is corrupt (sha256 {got}, runtime.json says {want})",
            p.display()
        )
        ));
    }
    Ok(p)
}

impl Runtime {
    /// Resolve and verify the vendored package. Errors name the missing or
    /// corrupt file.
    pub fn locate() -> Result<Runtime> {
        let dir = match std::env::var_os("PROJECTMP4_STEMS_RUNTIME") {
            Some(d) => PathBuf::from(d),
            None => Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/stems-runtime"),
        };
        Self::from_dir(&dir)
    }

    fn from_dir(dir: &Path) -> Result<Runtime> {
        let manifest = dir.join("runtime.json");
        let text = std::fs::read(&manifest)
            .map_err(|e| Error::msg(format!("stems runtime: cannot read {}: {e}", manifest.display())))?;
        let j: serde_json::Value = serde_json::from_slice(&text)
            .map_err(|e| Error::msg(format!("stems runtime: {} is not JSON: {e}", manifest.display())))?;
        let field = |v: &serde_json::Value, k: &str| -> Result<String> {
            v.get(k)
                .and_then(|x| x.as_str())
                .map(str::to_owned)
                .ok_or_else(|| Error::msg(format!("stems runtime: {} lacks string field {k:?}", manifest.display())))
        };
        if j.get("schema").and_then(|x| x.as_u64()) != Some(1) {
            return Err(Error::msg(format!("stems runtime: {} schema must be 1", manifest.display())));
        }
        let identity = field(&j, "identity")?;
        if identity.len() != 64 || !identity.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::msg(format!("stems runtime: {} identity is not 64 hex chars", manifest.display())));
        }
        let worker = verify(dir, &field(&j, "worker")?, &field(&j, "workerSha256")?)?;
        let model = verify(dir, &field(&j, "model")?, &field(&j, "modelSha256")?)?;
        let libs = j
            .get("libraries")
            .and_then(|x| x.as_array())
            .ok_or_else(|| Error::msg(format!("stems runtime: {} lacks libraries", manifest.display())))?;
        for lib in libs {
            verify(dir, &field(lib, "path")?, &field(lib, "sha256")?)?;
        }
        Ok(Runtime { worker, model })
    }
}

/// Weight of output frame `frame` of a span of `frames` frames starting at
/// `start`, within a source of `source_frames`. Unity at the song's bounds.
pub fn span_weight(frame: usize, frames: usize, start: usize, source_frames: usize) -> f64 {
    let mut w = 1.0;
    if start > 0 && frame < OVERLAP {
        w *= 0.5 - 0.5 * (PI * frame as f64 / OVERLAP as f64).cos();
    }
    if start + frames < source_frames && frame + OVERLAP >= frames {
        w *= 0.5 - 0.5 * (PI * (frames - 1 - frame) as f64 / OVERLAP as f64).cos();
    }
    w
}

/// Starts of the span schedule: `ceil(source_frames / HOP)` entries. A source
/// a frame or two past a hop multiple yields a ~1-frame final span, which the
/// worker's own alignment check may reject; agent-deck's schedule behaves
/// identically and bit-exactness requires matching it.
pub fn span_starts(source_frames: usize) -> impl Iterator<Item = usize> {
    (0..source_frames).step_by(STEM_HOP_FRAMES)
}

struct Report {
    decoded_frames: usize,
}

fn parse_report(stdout: &str) -> Result<Report> {
    let get = |key: &str| -> Result<&str> {
        stdout
            .lines()
            .filter_map(|l| l.trim().strip_prefix(key).and_then(|r| r.strip_prefix('=')))
            .next_back()
            .map(str::trim)
            .ok_or_else(|| Error::msg(format!("stems worker: stdout lacks {key}")))
    };
    let bad = |key: &str, v: &str| Error::msg(format!("stems worker: unparseable {key}={v}"));
    let n = get("decoded_frames_48k")?;
    let decoded_frames: usize = n.parse().map_err(|_| bad("decoded_frames_48k", n))?;
    if !(1..=MAX_SOURCE_FRAMES).contains(&decoded_frames) {
        return Err(Error::msg(format!("stems worker: decoded_frames_48k={decoded_frames} out of range")));
    }
    let d = get("alignment_delay_44100")?;
    let delay: i64 = d.parse().map_err(|_| bad("alignment_delay_44100", d))?;
    if delay.abs() >= 1024 {
        return Err(Error::msg(format!("stems worker: alignment_delay_44100={delay} is out of range")));
    }
    let c = get("alignment_correlation")?;
    let corr: f64 = c.parse().map_err(|_| bad("alignment_correlation", c))?;
    if !(corr >= 0.9) {
        return Err(Error::msg(format!("stems worker: alignment_correlation={corr} is below 0.9")));
    }
    let s = get("alignment_silent")?;
    if s != "0" && s != "1" {
        return Err(bad("alignment_silent", s));
    }
    Ok(Report { decoded_frames })
}

fn run_span(rt: &Runtime, source: &Path, out: &Path, start: usize) -> Result<Report> {
    std::fs::create_dir_all(out)
        .map_err(|e| Error::msg(format!("stems: cannot create {}: {e}", out.display())))?;
    let o = Command::new(&rt.worker)
        .arg(source)
        .arg(&rt.model)
        .arg(out)
        .arg(start.to_string())
        .arg(STEM_SPAN_FRAMES.to_string())
        .output()
        .map_err(|e| Error::msg(format!("stems: cannot spawn {}: {e}", rt.worker.display())))?;
    if !o.status.success() {
        return Err(Error::msg(format!(
            "stems worker failed at start={start} ({}): {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        )
        ));
    }
    parse_report(&String::from_utf8_lossy(&o.stdout))
        .map_err(|e| Error::msg(format!("span start={start}: {e}")))
}

/// Read a canonical 44-byte-header 48 kHz stereo f32 WAV of exactly `frames` frames.
fn read_wav(path: &Path, frames: usize) -> Result<Vec<f32>> {
    let b = std::fs::read(path).map_err(|e| Error::msg(format!("stems: cannot read {}: {e}", path.display())))?;
    let u16le = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u32le = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    if b.len() < 44 || &b[..4] != b"RIFF" || &b[36..40] != b"data" {
        return Err(Error::msg(format!("stems: {} has no canonical WAV header", path.display())));
    }
    if (u16le(20), u16le(22), u32le(24), u16le(34)) != (3, 2, 48000, 32) {
        return Err(Error::msg(format!("stems: {} is not 48 kHz stereo f32", path.display())));
    }
    if u32le(40) as usize != b.len() - 44 || b.len() - 44 != frames * 8 {
        return Err(Error::msg(format!(
            "stems: {} holds {} bytes of data, expected {} ({frames} frames)",
            path.display(),
            b.len() - 44,
            frames * 8
        )
        ));
    }
    Ok(b[44..].chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

const BLOCK: usize = 4096;

/// Separate `source` into four stems. `scratch` is a caller-owned directory
/// for per-span worker output (`span/<start>/`, about 110 MB per minute of
/// audio, kept until the caller removes it).
///
/// Streaming WOLA as in agent-deck's `stitch`: spans are run first, then the
/// output is walked in 4096-frame blocks holding only the spans that cover the
/// current block (at most two, since OVERLAP < HOP). The returned `Stems` own
/// four whole-track buffers (32 B/frame); that is the interface, and nothing
/// else scales with the source length.
pub fn separate(rt: &Runtime, source: &Path, scratch: &Path) -> Result<Stems> {
    let dir_of = |start: usize| scratch.join("span").join(start.to_string());
    let first = run_span(rt, source, &dir_of(0), 0)?;
    let source_frames = first.decoded_frames;
    let starts: Vec<usize> = span_starts(source_frames).collect();
    for &start in &starts[1..] {
        let r = run_span(rt, source, &dir_of(start), start)?;
        if r.decoded_frames != source_frames {
            return Err(Error::msg(format!(
                "stems worker: decoded_frames_48k changed from {source_frames} to {} at start={start}",
                r.decoded_frames
            )));
        }
    }
    let span_len = |start: usize| STEM_SPAN_FRAMES.min(source_frames - start);

    let mut out: [Vec<f32>; 4] = std::array::from_fn(|_| vec![0.0; source_frames * 2]);
    // Per role: the decoded PCM of spans that may still cover upcoming blocks.
    let mut cache: [Vec<(usize, Vec<f32>)>; 4] = std::array::from_fn(|_| Vec::new());
    let mut acc = vec![0.0f64; BLOCK * 2];
    let mut wsum = vec![0.0f64; BLOCK];

    let mut b0 = 0;
    while b0 < source_frames {
        let b1 = (b0 + BLOCK).min(source_frames);
        let n = b1 - b0;
        let covering: Vec<usize> = starts
            .iter()
            .copied()
            .filter(|&s| s < b1 && s + span_len(s) > b0)
            .collect();
        // Weights are identical for every role and both channels.
        // Indexed by block offset; zero where the span does not cover the frame.
        let weights: Vec<Vec<f64>> = covering
            .iter()
            .map(|&s| {
                (b0..b1)
                    .map(|f| match f.checked_sub(s) {
                        Some(k) if k < span_len(s) => span_weight(k, span_len(s), s, source_frames),
                        _ => 0.0,
                    })
                    .collect()
            })
            .collect();
        wsum[..n].fill(0.0);
        for w in &weights {
            for (t, v) in wsum[..n].iter_mut().zip(w) {
                *t += v;
            }
        }
        if let Some(f) = wsum[..n].iter().position(|w| !(*w > 0.0)) {
            return Err(Error::msg(format!("stems: no span weight covers output frame {}", b0 + f)));
        }
        for (r, role) in ROLES.iter().enumerate() {
            cache[r].retain(|(s, _)| s + span_len(*s) > b0);
            for &s in &covering {
                if !cache[r].iter().any(|(c, _)| *c == s) {
                    let pcm = read_wav(&dir_of(s).join(format!("{role}.wav")), span_len(s))?;
                    cache[r].push((s, pcm));
                }
            }
            acc[..n * 2].fill(0.0);
            // Ascending span order, as in the reference.
            for (&s, w) in covering.iter().zip(&weights) {
                let pcm = &cache[r].iter().find(|(c, _)| *c == s).unwrap().1;
                for i in 0..n {
                    if w[i] == 0.0 {
                        continue; // outside the span, or an exact-zero fade edge
                    }
                    let o = (b0 + i - s) * 2;
                    acc[i * 2] += w[i] * pcm[o] as f64;
                    acc[i * 2 + 1] += w[i] * pcm[o + 1] as f64;
                }
            }
            for i in 0..n {
                out[r][(b0 + i) * 2] = (acc[i * 2] / wsum[i]) as f32;
                out[r][(b0 + i) * 2 + 1] = (acc[i * 2 + 1] / wsum[i]) as f32;
            }
        }
        b0 = b1;
    }
    let [bass, drums, other, vocals] = out;
    Ok(Stems { bass, drums, other, vocals, frames: source_frames })
}
