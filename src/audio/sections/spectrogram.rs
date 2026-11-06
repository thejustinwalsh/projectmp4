//! Signal front end for the section model: 48 kHz mono PCM -> 44.1 kHz -> a
//! `[4, frames, 81]` log-band spectrogram. A port of agent-deck's
//! `Sinc48To44` (`wav48_stream.h`), `ssa_spectrogram_frame` and `make_filters`
//! (`song_structure.cpp`), and the framing loop in `worker.cpp`.

use crate::ffi::{Error, Result};
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::{Arc, OnceLock};

pub const BINS: usize = 81;
pub const HOP: usize = 441;
pub const SR: u32 = 44100;
const WIN: usize = 2048;
const HALF: usize = 1024;
const TAPS: usize = 64;
const PHASES: usize = 147;

/// 64-tap Hann-windowed sinc, one row per output phase, built in `f64` and
/// normalised to unit sum per row.
fn taps() -> &'static [[f64; TAPS]; PHASES] {
    static T: OnceLock<Box<[[f64; TAPS]; PHASES]>> = OnceLock::new();
    T.get_or_init(|| {
        let pi = std::f64::consts::PI;
        let cutoff = 0.5 * 147.0 / 160.0;
        let mut t = Box::new([[0f64; TAPS]; PHASES]);
        for (phase, row) in t.iter_mut().enumerate() {
            let mut sum = 0.0;
            for (tap, w) in row.iter_mut().enumerate() {
                let d = phase as f64 / 147.0 - (tap as f64 - 31.0);
                let window = 0.5 + 0.5 * (pi * d / 32.0).cos();
                let arg = 2.0 * cutoff * d;
                let sinc = if arg.abs() < 1e-12 { 1.0 } else { (pi * arg).sin() / (pi * arg) };
                *w = 2.0 * cutoff * sinc * window;
                sum += *w;
            }
            for w in row.iter_mut() {
                *w /= sum;
            }
        }
        t
    })
}

/// Resample mono 48 kHz to 44.1 kHz. Output length is `(n*147 + 159) / 160`;
/// output 0 aligns with input 0 (zero group delay); source indices clamp at both ends.
pub fn resample_48k_to_44k1(input: &[f32]) -> Result<Vec<f32>> {
    if let Some(i) = input.iter().position(|v| !v.is_finite()) {
        return Err(Error::msg(format!("non-finite PCM at source sample {i}")));
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let t = taps();
    let last = input.len() as i64 - 1;
    let out_len = (input.len() as u64 * 147 + 159) / 160;
    let mut out = Vec::with_capacity(out_len as usize);
    for n in 0..out_len {
        // Recomputed from the output index every time: never accumulated.
        let num = n * 160;
        let center = (num / 147) as i64;
        let w = &t[(num % 147) as usize];
        let mut sum = 0f64;
        for (k, wk) in w.iter().enumerate() {
            let idx = (center + k as i64 - 31).clamp(0, last) as usize;
            sum += wk * input[idx] as f64;
        }
        out.push(sum as f32);
    }
    Ok(out)
}

/// Spectrogram frames for `samples` of 44.1 kHz audio: centred, hop 441.
pub fn frames_for(samples: usize) -> usize {
    (samples + HOP - 1) / HOP
}

/// 81 triangular rows over FFT bins, `(start, row)` with the row spanning
/// `start..start+row.len()`. Zero outside that span, as in the reference.
struct Filters(Vec<(usize, Vec<f32>)>);

fn filters() -> Result<&'static Filters> {
    static F: OnceLock<std::result::Result<Filters, String>> = OnceLock::new();
    F.get_or_init(make_filters).as_ref().map_err(|e| Error::msg(e.clone()))
}

fn make_filters() -> std::result::Result<Filters, String> {
    let mut bins: Vec<usize> = Vec::new();
    let left = ((30.0f64 / 440.0).log2() * 12.0).floor() as i32;
    let right = ((17000.0f64 / 440.0).log2() * 12.0).ceil() as i32;
    for i in left..right {
        let f = 440.0 * 2f64.powf(i as f64 / 12.0);
        if f < 30.0 || f > 17000.0 {
            continue;
        }
        let pos = f * 2048.0 / 44100.0;
        let bin = ((pos + 0.5).floor() as i64).clamp(1, 1023) as usize;
        if bins.last() != Some(&bin) {
            bins.push(bin);
        }
    }
    if bins.len() != 83 {
        return Err(format!("unexpected logarithmic filter bank dimensions: {}", bins.len()));
    }
    let mut rows = Vec::with_capacity(BINS);
    for b in 0..BINS {
        let (start, center, stop) = (bins[b], bins[b + 1], bins[b + 2]);
        let mut sum = 0f32;
        let mut row = Vec::with_capacity(stop - start);
        for j in start..stop {
            let a = if j < center {
                (j - start) as f32 / (center - start) as f32
            } else {
                1.0 - (j - center) as f32 / (stop - center) as f32
            };
            row.push(a);
            sum += a;
        }
        for a in row.iter_mut() {
            *a /= sum;
        }
        rows.push((start, row));
    }
    Ok(Filters(rows))
}

fn window() -> &'static [f32; WIN] {
    static W: OnceLock<Box<[f32; WIN]>> = OnceLock::new();
    W.get_or_init(|| {
        let mut w = Box::new([0f32; WIN]);
        for (i, v) in w.iter_mut().enumerate() {
            *v = (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / 2047.0).cos()) as f32;
        }
        w
    })
}

fn plan() -> &'static Arc<dyn Fft<f64>> {
    static P: OnceLock<Arc<dyn Fft<f64>>> = OnceLock::new();
    P.get_or_init(|| FftPlanner::<f64>::new().plan_fft_forward(WIN))
}

/// One 2048-sample window -> 81 `log10(1 + band magnitude)` values.
/// Hann window, unnormalised forward FFT, magnitudes over bins `0..1024`.
pub fn frame_bands(samples_2048: &[f32; 2048], out: &mut [f32; 81]) -> Result<()> {
    if let Some(i) = samples_2048.iter().position(|v| !v.is_finite()) {
        return Err(Error::msg(format!("non-finite PCM at window index {i}")));
    }
    let fl = filters()?;
    let w = window();
    let mut x: Vec<Complex<f64>> = samples_2048
        .iter()
        .zip(w.iter())
        .map(|(s, w)| Complex::new((s * w) as f64, 0.0))
        .collect();
    plan().process(&mut x);
    let mag: Vec<f64> = x[..HALF].iter().map(|c| c.norm()).collect();
    for (o, (start, row)) in out.iter_mut().zip(fl.0.iter()) {
        let mut sum = 0f64;
        for (k, a) in row.iter().enumerate() {
            sum += mag[start + k] * *a as f64;
        }
        *o = (sum + 1.0).log10() as f32;
    }
    Ok(())
}

/// Four equal-length mono 44.1 kHz stems (bass, drums, other, vocals) ->
/// contiguous `[4, frames, 81]` and the frame count. Windows are streamed: only
/// one 2048-sample window is held at a time. Samples outside the stem are zero.
pub fn stack(stems: &[Vec<f32>; 4]) -> Result<(Vec<f32>, usize)> {
    let n = stems[0].len();
    if stems.iter().any(|s| s.len() != n) {
        return Err(Error::msg("stems differ in length"));
    }
    if n == 0 {
        return Err(Error::msg("stems are empty"));
    }
    let frames = frames_for(n);
    let mut out = vec![0f32; 4 * frames * BINS];
    let mut win = [0f32; WIN];
    let mut bands = [0f32; BINS];
    for (k, stem) in stems.iter().enumerate() {
        for t in 0..frames {
            let start = (t * HOP) as i64 - 1024;
            let begin = start.max(0);
            let end = (start + WIN as i64).min(n as i64);
            win.fill(0.0);
            win[(begin - start) as usize..(end - start) as usize]
                .copy_from_slice(&stem[begin as usize..end as usize]);
            frame_bands(&win, &mut bands)
                .map_err(|e| Error::msg(format!("stem {k} frame {t}: {}", e.0)))?;
            let o = (k * frames + t) * BINS;
            out[o..o + BINS].copy_from_slice(&bands);
        }
    }
    Ok((out, frames))
}
