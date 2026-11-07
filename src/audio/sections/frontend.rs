//! Four-channel front ends for the section model, derived from a mono mix alone.
//!
//! The model was trained on four htdemucs stems (bass, drums, other, vocals). A front end
//! maps mono PCM to four equal-length channels in that slot order. **These are
//! approximations of htdemucs stems, not separations**: nothing here knows what a voice or
//! a bass guitar is. How much the section labels degrade is measured in
//! `examples/section_frontends.rs`, not assumed.
//!
//! The control that reads real stems from agent-deck's cache lives in `tests/common`, outside
//! this crate's library, so no build of the library or the binary can reach that cache.

use crate::ffi::{Error, Result};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

/// Slot order is the model's: bass, drums, other, vocals.
pub trait FourChannel {
    /// `mono` at `sr` Hz in; four channels of exactly `mono.len()` samples at the same rate out.
    fn channels(&self, mono: &[f32], sr: u32) -> Result<[Vec<f32>; 4]>;
}

/// The mix in all four slots.
pub struct MixFourWays;

impl FourChannel for MixFourWays {
    fn channels(&self, mono: &[f32], _sr: u32) -> Result<[Vec<f32>; 4]> {
        finite(mono)?;
        Ok([mono.to_vec(), mono.to_vec(), mono.to_vec(), mono.to_vec()])
    }
}

/// Harmonic/percussive split by median filtering (Fitzgerald 2010), then a frequency split
/// of the harmonic part.
///
/// * drums  = percussive signal, full band
/// * bass   = harmonic signal below 250 Hz
/// * other  = harmonic signal, 250 Hz - 4 kHz
/// * vocals = harmonic signal, 200 Hz - 4 kHz
///
/// The input is mono, so there is no centre channel to emphasise: `vocals` and `other` are
/// the same harmonic mid-band apart from the 200-250 Hz sliver.
pub struct HpssBands;

const N: usize = 2048;
const HOP: usize = N / 4;
const BINS: usize = N / 2 + 1;
/// Median kernel length, in frames (time) and in bins (frequency). librosa's default.
const KERNEL: usize = 31;
const R: usize = KERNEL / 2;

fn finite(x: &[f32]) -> Result<()> {
    match x.iter().position(|v| !v.is_finite()) {
        Some(i) => Err(Error::msg(format!("non-finite PCM at sample {i}"))),
        None => Ok(()),
    }
}

fn hann() -> Vec<f32> {
    (0..N).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / N as f64).cos()) as f32).collect()
}

/// Median of `v` (destroys order).
fn median(v: &mut [f32]) -> f32 {
    let m = v.len() / 2;
    *v.select_nth_unstable_by(m, |a, b| a.total_cmp(b)).1
}

impl FourChannel for HpssBands {
    fn channels(&self, mono: &[f32], sr: u32) -> Result<[Vec<f32>; 4]> {
        finite(mono)?;
        let n = mono.len();
        if n == 0 {
            return Ok([vec![], vec![], vec![], vec![]]);
        }
        if sr == 0 {
            return Err(Error::msg("sample rate is 0"));
        }
        let mut planner = FftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(N);
        let inv = planner.plan_fft_inverse(N);
        let win = hann();
        // Frame t spans samples start(t)..start(t)+N; every sample sits under exactly four.
        let start = |t: usize| t as i64 * HOP as i64 - (N - HOP) as i64;
        let frames = (n + N - HOP + HOP - 1) / HOP;
        let frame_fft = |t: usize, buf: &mut Vec<Complex<f32>>| {
            let s = start(t);
            buf.clear();
            for j in 0..N {
                let i = s + j as i64;
                let v = if i >= 0 && (i as usize) < n { mono[i as usize] } else { 0.0 };
                buf.push(Complex::new(v * win[j], 0.0));
            }
            fwd.process(buf);
        };

        // Pass 1: magnitudes, [frames][BINS].
        let mut mag = vec![0f32; frames * BINS];
        let mut buf = Vec::with_capacity(N);
        for t in 0..frames {
            frame_fft(t, &mut buf);
            for k in 0..BINS {
                mag[t * BINS + k] = buf[k].norm();
            }
        }

        // Pass 2: harmonic = median across time (per bin), percussive = median across
        // frequency (per frame). Windows truncate at the edges.
        let mut harm = vec![0f32; frames * BINS];
        let mut col = vec![0f32; frames];
        let mut tmp = [0f32; KERNEL];
        for k in 0..BINS {
            for t in 0..frames {
                col[t] = mag[t * BINS + k];
            }
            for t in 0..frames {
                let (lo, hi) = (t.saturating_sub(R), (t + R + 1).min(frames));
                let w = &mut tmp[..hi - lo];
                w.copy_from_slice(&col[lo..hi]);
                harm[t * BINS + k] = median(w);
            }
        }
        let mut perc = vec![0f32; frames * BINS];
        for t in 0..frames {
            let row = &mag[t * BINS..(t + 1) * BINS];
            for k in 0..BINS {
                let (lo, hi) = (k.saturating_sub(R), (k + R + 1).min(BINS));
                let w = &mut tmp[..hi - lo];
                w.copy_from_slice(&row[lo..hi]);
                perc[t * BINS + k] = median(w);
            }
        }
        drop(mag);

        // Pass 3: soft masks (power 2) applied to the complex spectrum, one band mask per
        // slot, inverse transform, overlap-add. Periodic Hann at hop N/4 sums to 2.
        let hz = |f: f64| ((f * N as f64 / sr as f64).round() as usize).min(BINS);
        let (b250, b200, b4k) = (hz(250.0), hz(200.0), hz(4000.0));
        let mut out: [Vec<f32>; 4] = std::array::from_fn(|_| vec![0f32; n]);
        let mut spec: [Vec<Complex<f32>>; 4] = std::array::from_fn(|_| vec![Complex::new(0.0, 0.0); N]);
        for t in 0..frames {
            frame_fft(t, &mut buf);
            for k in 0..BINS {
                let (h, p) = (harm[t * BINS + k], perc[t * BINS + k]);
                let (h2, p2) = (h * h, p * p);
                let d = h2 + p2 + 1e-12;
                let (mh, mp) = (h2 / d, p2 / d);
                let x = buf[k];
                let v = [
                    if k < b250 { x * mh } else { Complex::new(0.0, 0.0) },
                    x * mp,
                    if k >= b250 && k < b4k { x * mh } else { Complex::new(0.0, 0.0) },
                    if k >= b200 && k < b4k { x * mh } else { Complex::new(0.0, 0.0) },
                ];
                for (s, v) in spec.iter_mut().zip(v) {
                    s[k] = v;
                    if k > 0 && k < N / 2 {
                        s[N - k] = v.conj();
                    }
                }
            }
            let s0 = start(t);
            for (slot, s) in spec.iter_mut().enumerate() {
                inv.process(s);
                for j in 0..N {
                    let i = s0 + j as i64;
                    if i >= 0 && (i as usize) < n {
                        out[slot][i as usize] += s[j].re / (N as f32 * 2.0);
                    }
                }
            }
        }
        Ok(out)
    }
}
