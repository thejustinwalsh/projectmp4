//! Forward pass of the All-In-One "harmonix" section network, in pure Rust.
//!
//! This reproduces `native/structure-analysis/src/song_structure.cpp` (`ssa_predict`),
//! single-threaded and batch-1, with the same per-element operation order (f32, no fused
//! multiply-add, ascending reductions) so the output tracks the reference to ~1e-6.
//!
//! Several behaviours below look like bugs and are not: they are what the reference
//! computes, and the parity gate depends on them. Each is marked `DELIBERATE`.
//!
//! Layout: activations are row-major `[stem 4, time, channel 24]`. Weight shapes are the
//! row-major numpy shapes `Weights::tensor` returns (GGUF dims are reversed by the reader).

use super::gguf::Weights;
use crate::ffi::{Error, Result};
use std::sync::OnceLock;

const K: usize = 4; // stems
const C: usize = 24; // channels
const HEADS: usize = 2;
const HD: usize = 12; // head dim
const KERNEL: usize = 5; // attention window
const MAX_FRAMES: usize = 1_000_000;
const LN_EPS: f32 = 1e-5;
/// Rows processed per MLP chunk. Per-row results do not depend on the chunking.
const MLP_CHUNK: usize = 2048;

/// Largest working set `predict` will attempt, by [`estimated_working_bytes`]: 2 GiB, about
/// 87,000 frames (14.5 minutes at 100 fps). The estimate is the reference's own formula and
/// is roughly 3x what this implementation actually touches, so it errs on the safe side.
pub const MAX_WORKING_BYTES: u64 = 2 << 30;

/// The reference's `ssa_estimated_working_bytes`: 16 MiB + max(frames, 10240) * 24576.
/// 10240 is the padded length of the deepest time layer (`10 * 1024`).
pub fn estimated_working_bytes(frames: usize) -> u64 {
    (16u64 << 20) + (frames.max(10240) as u64) * 24576
}

// ---------------------------------------------------------------- weights

fn tensor<'a>(w: &'a Weights, name: &str, shape: &[usize]) -> Result<&'a [f32]> {
    let (data, got) = w.tensor(name)?;
    if got != shape {
        return Err(Error::msg(format!("model: tensor {name:?} has shape {got:?}, expected {shape:?}")));
    }
    Ok(data)
}

/// A linear layer with its weight transposed to `[in, out]`, so the inner loop runs over
/// outputs (vectorisable) while each output still accumulates `bias + a[0]*w[0] + a[1]*w[1]
/// + ...` in ascending `i`, exactly like the reference.
struct Lin<'a> {
    wt: Vec<f32>,
    b: &'a [f32],
    inn: usize,
    out: usize,
}

impl<'a> Lin<'a> {
    fn load(w: &'a Weights, name: &str, inn: usize, out: usize) -> Result<Lin<'a>> {
        let src = tensor(w, &format!("{name}.weight"), &[out, inn])?;
        let b = tensor(w, &format!("{name}.bias"), &[out])?;
        let mut wt = vec![0f32; inn * out];
        for o in 0..out {
            for i in 0..inn {
                wt[i * out + o] = src[o * inn + i];
            }
        }
        Ok(Lin { wt, b, inn, out })
    }

    fn apply(&self, x: &[f32], rows: usize, y: &mut [f32]) {
        debug_assert!(x.len() >= rows * self.inn && y.len() >= rows * self.out);
        for r in 0..rows {
            let yr = &mut y[r * self.out..(r + 1) * self.out];
            yr.copy_from_slice(self.b);
            let a = &x[r * self.inn..(r + 1) * self.inn];
            for (i, &ai) in a.iter().enumerate() {
                let wi = &self.wt[i * self.out..(i + 1) * self.out];
                for (o, yo) in yr.iter_mut().enumerate() {
                    *yo += ai * wi[o];
                }
            }
        }
    }

    fn run(&self, x: &[f32], rows: usize) -> Vec<f32> {
        let mut y = vec![0f32; rows * self.out];
        self.apply(x, rows, &mut y);
        y
    }
}

struct Norm<'a> {
    w: &'a [f32],
    b: &'a [f32],
}

impl<'a> Norm<'a> {
    fn load(w: &'a Weights, name: &str, width: usize) -> Result<Norm<'a>> {
        Ok(Norm { w: tensor(w, &format!("{name}.weight"), &[width])?, b: tensor(w, &format!("{name}.bias"), &[width])? })
    }

    /// Two-pass biased LayerNorm, epsilon added to the variance, as `norm()` in the reference.
    fn apply(&self, x: &[f32], y: &mut [f32]) {
        let n = self.w.len();
        let nf = n as f32;
        for (xr, yr) in x.chunks_exact(n).zip(y.chunks_exact_mut(n)) {
            let mut mean = 0f32;
            for &v in xr {
                mean += v;
            }
            mean /= nf;
            let mut var = 0f32;
            for &v in xr {
                let d = v - mean;
                var += d * d;
            }
            let inv = 1.0f32 / (var / nf + LN_EPS).sqrt();
            for i in 0..n {
                yr[i] = (xr[i] - mean) * inv * self.w[i] + self.b[i];
            }
        }
    }

    fn run(&self, x: &[f32]) -> Vec<f32> {
        let mut y = vec![0f32; x.len()];
        self.apply(x, &mut y);
        y
    }
}

// ---------------------------------------------------------------- erf / GELU

/// erf(x) at the f32 precision libm's `erff` gives. Rust's std has no erf and a new
/// dependency is not allowed, so this tabulates erf and its derivative at 1/64 steps (built
/// once, from the Maclaurin series in f64) and Taylor-expands to third order about the
/// nearest node. Error is ~2e-9, about 30x below one f32 ulp near 1.
fn erf32(x: f32) -> f32 {
    const STEP: f64 = 1.0 / 64.0;
    const NODES: usize = 4 * 64 + 1;
    static TABLE: OnceLock<Vec<(f64, f64)>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let c = 2.0 / std::f64::consts::PI.sqrt();
        (0..NODES)
            .map(|i| {
                let x = i as f64 * STEP;
                let (mut term, mut sum) = (x, x);
                let mut n = 1.0f64;
                while term.abs() > 1e-20 {
                    term *= -x * x / n;
                    sum += term / (2.0 * n + 1.0);
                    n += 1.0;
                }
                (c * sum, c * (-x * x).exp())
            })
            .collect()
    });
    let a = (x as f64).abs();
    if a >= 4.0 {
        return 1f32.copysign(x);
    }
    let i = (a / STEP + 0.5) as usize;
    let x0 = i as f64 * STEP;
    let h = a - x0;
    let (e0, d0) = table[i];
    let v = e0 + d0 * (h - x0 * h * h + (4.0 * x0 * x0 - 2.0) / 6.0 * h * h * h);
    (v as f32).copysign(x)
}

fn gelu(a: f32) -> f32 {
    // Exact erf form, as the reference (not the tanh approximation).
    0.5f32 * a * (1.0f32 + erf32(a / (2.0f64.sqrt() as f32)))
}

// ---------------------------------------------------------------- embeddings

/// Valid conv along frequency, zero-padded "same" along time, max-pool 3 along frequency
/// (a remainder column is dropped), then ELU. `x` is `[4, frames, freq, inn]`.
#[allow(clippy::too_many_arguments)]
fn conv_pool(w: &Weights, x: &[f32], frames: usize, freq: usize, inn: usize, out: usize, kt: usize, kf: usize, name: &str) -> Result<Vec<f32>> {
    let src = tensor(w, &format!("{name}.weight"), &[out, kt, kf, inn])?;
    let bias = tensor(w, &format!("{name}.bias"), &[out])?;
    // [o][dt][df][c] -> [dt][df][c][o]
    let mut wt = vec![0f32; src.len()];
    for o in 0..out {
        for j in 0..kt * kf * inn {
            wt[j * out + o] = src[o * kt * kf * inn + j];
        }
    }
    let pool_f = (freq - kf + 1) / 3;
    let mut y = vec![0f32; K * frames * pool_f * out];
    let mut acc = [[0f32; C]; 3];
    for k in 0..K {
        for t in 0..frames {
            for f in 0..pool_f {
                for a in acc.iter_mut() {
                    a[..out].copy_from_slice(bias);
                }
                for dt in 0..kt {
                    let ti = t as isize + dt as isize - (kt / 2) as isize;
                    if ti < 0 || ti >= frames as isize {
                        continue; // zero padding: tap skipped
                    }
                    for df in 0..kf {
                        for c in 0..inn {
                            let wrow = &wt[((dt * kf + df) * inn + c) * out..][..out];
                            for (p, a) in acc.iter_mut().enumerate() {
                                let v = x[((k * frames + ti as usize) * freq + f * 3 + p + df) * inn + c];
                                for o in 0..out {
                                    a[o] += v * wrow[o];
                                }
                            }
                        }
                    }
                }
                let dst = &mut y[((k * frames + t) * pool_f + f) * out..][..out];
                for o in 0..out {
                    let m = acc[0][o].max(acc[1][o]).max(acc[2][o]);
                    dst[o] = if m >= 0.0 { m } else { m.exp_m1() };
                }
            }
        }
    }
    Ok(y)
}

// ---------------------------------------------------------------- attention

/// NATTEN shifted window start for position `i` on an axis of `length` with dilation `d`:
/// the window is 5 members of `i`'s residue class, clamped (shifted, not padded) at the edges.
fn window_start(i: usize, length: usize, d: usize) -> usize {
    let phase = i % d;
    let pos = (i / d) as isize;
    let mut len = ((length + d - 1) / d) as isize;
    let pad = len * d as isize - length as isize;
    if phase as isize >= d as isize - pad {
        len -= 1;
    }
    (pos - 2).clamp(0, len - KERNEL as isize) as usize * d + phase
}

/// Start of the 5 relative-position-bias indices for position `i`.
/// DELIBERATE: the two edge branches are the reference's mapping, not a centred offset.
fn bias_start(i: usize, length: usize, d: usize) -> usize {
    if i < 2 * d {
        return 4 - i / d;
    }
    if i + 2 * d >= length {
        return (length - i - 1) / d;
    }
    2
}

/// Neighbourhood attention over `pad`, `[height, padded, 24]` (height 4 for time layers, 5
/// for the stem+time layer), returning only the real rows: `[4, t_real, 24]`, projected by
/// `output.dense`.
///
/// DELIBERATE (geometry): `padded`, not `t_real`, is the axis length for `window_start` and
/// `bias_start`, so windows and bias indices near the real tail see the padded length.
/// DELIBERATE (live pad): the zero padding rows (and the all-zero 5th stem row) are ordinary
/// keys and values, equal to `key.bias` / `value.bias`. They are not masked.
#[allow(clippy::too_many_arguments)]
fn attention(w: &Weights, pad: &[f32], height: usize, padded: usize, t_real: usize, dilation: usize, two_d: bool, name: &str) -> Result<Vec<f32>> {
    let q_l = Lin::load(w, &format!("{name}.self.query"), C, C)?;
    let k_l = Lin::load(w, &format!("{name}.self.key"), C, C)?;
    let v_l = Lin::load(w, &format!("{name}.self.value"), C, C)?;
    let o_l = Lin::load(w, &format!("{name}.output.dense"), C, C)?;
    let rpb = tensor(w, &format!("{name}.self.rpb"), if two_d { &[HEADS, 9, 9] } else { &[HEADS, 9] })?;

    let key = k_l.run(pad, height * padded);
    let val = v_l.run(pad, height * padded);
    // Queries only for the rows that survive (k < 4, t < t_real); a pad row's query never
    // reaches a real row, so skipping it does not change any output.
    let mut q = vec![0f32; K * t_real * C];
    for k in 0..K {
        q_l.apply(&pad[k * padded * C..], t_real, &mut q[k * t_real * C..]);
    }
    // Scale by division per element, as the reference does (not a multiply by a reciprocal).
    let scale = (HD as f32).sqrt();
    for v in q.iter_mut() {
        *v /= scale;
    }

    let neighbors = if two_d { 25 } else { KERNEL };
    let mut result = vec![0f32; K * t_real * C];
    let mut scores = [0f32; 25];
    let mut rows = [0usize; 25];
    for k in 0..K {
        let (ks, kb) = if two_d { (window_start(k, height, 1), bias_start(k, height, 1)) } else { (k, 0) };
        for t in 0..t_real {
            let ts = window_start(t, padded, dilation);
            let tb = bias_start(t, padded, dilation);
            for h in 0..HEADS {
                let qo = (k * t_real + t) * C + h * HD;
                let mut maximum = f32::NEG_INFINITY;
                let mut n = 0;
                for dk in 0..(if two_d { 5 } else { 1 }) {
                    for dt in 0..KERNEL {
                        let ki = if two_d { ks + dk } else { k };
                        let ti = ts + dt * dilation;
                        rows[n] = ki * padded + ti;
                        let mut sum = 0f32;
                        for c in 0..HD {
                            sum += q[qo + c] * key[rows[n] * C + h * HD + c];
                        }
                        sum += rpb[if two_d { (h * 9 + kb + dk) * 9 + tb + dt } else { h * 9 + tb + dt }];
                        scores[n] = sum;
                        maximum = maximum.max(sum);
                        n += 1;
                    }
                }
                let mut total = 0f32;
                for s in scores.iter_mut().take(neighbors) {
                    *s = (*s - maximum).exp();
                    total += *s;
                }
                for s in scores.iter_mut().take(neighbors) {
                    *s /= total;
                }
                for c in 0..HD {
                    let mut sum = 0f32;
                    for a in 0..neighbors {
                        sum += scores[a] * val[rows[a] * C + h * HD + c];
                    }
                    result[qo + c] = sum;
                }
            }
        }
    }
    Ok(o_l.run(&result, K * t_real))
}

// ---------------------------------------------------------------- encoder layer

/// One `timelayer` (`two_d == false`: double 1-D attention, dilation `d` and `2d`) or
/// `instlayer` (`two_d == true`: one 5x5 stem x time attention). `x` is `[4, t, 24]`.
fn layer(w: &Weights, x: &[f32], t: usize, dilation: usize, two_d: bool, name: &str) -> Result<Vec<f32>> {
    let normalized = Norm::load(w, &format!("{name}.layernorm_before"), C)?.run(x);
    // DELIBERATE: padded length is max(t, 10*d) for time layers and max(t, 5) for the
    // stem layer, and every window below is computed against it.
    let padded = t.max(if two_d { 5 } else { 10 * dilation });
    // DELIBERATE: the stem layer has 5 rows; row 4 is all zero and is a live key and value.
    let height = if two_d { 5 } else { K };
    let mut pad = vec![0f32; height * padded * C];
    for k in 0..K {
        pad[k * padded * C..k * padded * C + t * C].copy_from_slice(&normalized[k * t * C..(k + 1) * t * C]);
    }
    drop(normalized);
    let first = attention(w, &pad, height, padded, t, dilation, two_d, &format!("{name}.attention"))?;
    let second = if two_d { None } else { Some(attention(w, &pad, height, padded, t, dilation * 2, false, &format!("{name}.attention2"))?) };
    drop(pad);

    let width = if two_d { C } else { 2 * C };
    let norm_after = Norm::load(w, &format!("{name}.layernorm_after"), width)?;
    let inter = Lin::load(w, &format!("{name}.intermediate.dense"), width, width * 4)?;
    let outd = Lin::load(w, &format!("{name}.output.dense"), width * 4, C)?;

    let rows = K * t;
    let mut out = vec![0f32; rows * C];
    let mut concat = vec![0f32; MLP_CHUNK * width];
    let mut normed = vec![0f32; MLP_CHUNK * width];
    let mut hidden = vec![0f32; MLP_CHUNK * width * 4];
    let mut shortcut = vec![0f32; MLP_CHUNK * C];
    let mut r0 = 0;
    while r0 < rows {
        let n = MLP_CHUNK.min(rows - r0);
        for r in 0..n {
            for c in 0..C {
                let i = (r0 + r) * C + c;
                let a = x[i] + first[i];
                // In the stem layer b == a, so shortcut == (a+a)*0.5 == a exactly.
                let b = match &second {
                    Some(s) => x[i] + s[i],
                    None => a,
                };
                shortcut[r * C + c] = (a + b) * 0.5f32;
                concat[r * width + c] = a;
                if !two_d {
                    concat[r * width + C + c] = b;
                }
            }
        }
        norm_after.apply(&concat[..n * width], &mut normed[..n * width]);
        inter.apply(&normed, n, &mut hidden);
        for v in hidden[..n * width * 4].iter_mut() {
            *v = gelu(*v);
        }
        let o = &mut out[r0 * C..(r0 + n) * C];
        outd.apply(&hidden, n, o);
        for (ov, sv) in o.iter_mut().zip(&shortcut) {
            *ov += *sv;
        }
        r0 += n;
    }
    Ok(out)
}

// ---------------------------------------------------------------- predict

/// Logits for a `[4, frames, 81]` spectrogram: `[frames, 11]`, column 0 the section
/// boundary logit and columns 1..=10 the function-class logits (see `LABELS`).
pub fn predict(w: &Weights, spec: &[f32], frames: usize) -> Result<Vec<f32>> {
    if frames == 0 {
        return Err(Error::msg("model: frames is 0"));
    }
    if frames > MAX_FRAMES {
        return Err(Error::msg(format!("model: {frames} frames exceeds the {MAX_FRAMES}-frame limit")));
    }
    let need = estimated_working_bytes(frames);
    if need > MAX_WORKING_BYTES {
        return Err(Error::msg(format!(
            "model: {frames} frames need an estimated {need} bytes of working memory, over the {MAX_WORKING_BYTES}-byte ceiling"
        )));
    }
    if spec.len() != K * frames * 81 {
        return Err(Error::msg(format!("model: spectrogram has {} values, expected 4*{frames}*81 = {}", spec.len(), K * frames * 81)));
    }
    if spec.iter().any(|v| !v.is_finite()) {
        return Err(Error::msg("model: spectrogram contains a non-finite value"));
    }
    // Settle the layout before building on it: GGUF dims are reversed to row-major by the reader.
    tensor(w, "embeddings.conv1.weight", &[24, 1, 12, 12])?;

    let t = frames;
    let mut x = conv_pool(w, spec, t, 81, 1, 12, 3, 3, "embeddings.conv0")?;
    x = conv_pool(w, &x, t, 26, 12, 24, 1, 12, "embeddings.conv1")?;
    x = conv_pool(w, &x, t, 5, 24, 24, 3, 3, "embeddings.conv2")?;
    x = Norm::load(w, "embeddings.norm", C)?.run(&x);
    for i in 0..11 {
        let name = format!("encoder.layers.{i}");
        x = layer(w, &x, t, 1 << i, false, &format!("{name}.timelayer"))?;
        x = layer(w, &x, t, 1, true, &format!("{name}.instlayer"))?;
    }
    x = Norm::load(w, "norm", C)?.run(&x);

    // Merge stems: flat[f][k*24 + c] = x[k][f][c].
    let mut flat = vec![0f32; frames * K * C];
    for k in 0..K {
        for f in 0..frames {
            flat[(f * K + k) * C..(f * K + k + 1) * C].copy_from_slice(&x[(k * frames + f) * C..(k * frames + f + 1) * C]);
        }
    }
    drop(x);
    let section = Lin::load(w, "section_classifier.classifier", K * C, 1)?.run(&flat, frames);
    let function = Lin::load(w, "function_classifier.classifier", K * C, 10)?.run(&flat, frames);
    let mut out = vec![0f32; frames * 11];
    for f in 0..frames {
        out[f * 11] = section[f];
        out[f * 11 + 1..f * 11 + 11].copy_from_slice(&function[f * 10..f * 10 + 10]);
    }
    if out.iter().any(|v| !v.is_finite()) {
        return Err(Error::msg("model: non-finite prediction"));
    }
    Ok(out)
}
