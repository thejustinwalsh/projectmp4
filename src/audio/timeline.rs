//! When the visuals change preset. Switches land on downbeats; a drop earns a
//! hard cut sooner than the normal spacing allows.
//!
//! The gap clock starts at `t = 0` and the opening preset counts as the switch
//! at `t = 0`: a change 120 ms into a track would replace a preset nobody has
//! seen yet.

use crate::audio::{Analysis, Driver};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    Hard,
    Soft,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SwitchPoint {
    pub t: f64,
    pub cut: Cut,
}

/// One-pole low-pass corner of the "low band".
const LOW_BAND_HZ: f64 = 200.0;
/// Envelope hop, in frames (about 21 ms at 48 kHz).
const HOP: usize = 1024;
/// Seconds of envelope averaged on each side of a downbeat to measure its step.
const STEP_WINDOW_S: f64 = 1.0;
/// A downbeat is a drop when its rise in low-band energy exceeds this many
/// times the median absolute step over all downbeats.
pub const DROP_FACTOR: f64 = 4.0;

/// Mean-square energy of the sub-200 Hz mono signal, one value per `HOP` frames.
fn low_band_envelope(d: &Driver) -> Vec<f64> {
    let s = d.samples();
    let a = 1.0 - (-2.0 * std::f64::consts::PI * LOW_BAND_HZ / d.rate as f64).exp();
    let mut y = 0.0f64;
    let mut env = Vec::with_capacity(d.frames() / HOP + 1);
    for chunk in s.chunks(HOP * 2) {
        let mut acc = 0.0;
        for f in chunk.chunks_exact(2) {
            y += a * ((f[0] as f64 + f[1] as f64) * 0.5 - y);
            acc += y * y;
        }
        env.push(acc / (chunk.len() / 2).max(1) as f64);
    }
    env
}

fn mean_over(env: &[f64], from_s: f64, to_s: f64, hop_s: f64) -> f64 {
    let lo = ((from_s.max(0.0) / hop_s) as usize).min(env.len());
    let hi = ((to_s.max(0.0) / hop_s).ceil() as usize).min(env.len());
    if hi <= lo {
        return 0.0;
    }
    env[lo..hi].iter().sum::<f64>() / (hi - lo) as f64
}

/// Rise in low-band energy across each downbeat (after minus before).
fn downbeat_steps(a: &Analysis, d: &Driver) -> Vec<f64> {
    let env = low_band_envelope(d);
    let hop_s = HOP as f64 / d.rate as f64;
    a.downbeats
        .iter()
        .map(|&t| {
            mean_over(&env, t, t + STEP_WINDOW_S, hop_s)
                - mean_over(&env, t - STEP_WINDOW_S, t, hop_s)
        })
        .collect()
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// Switch points: `min_gap` seconds apart at least, or `drop_gap` for a drop
/// (which is also marked `Hard`). Every point is a downbeat, `< a.duration`.
pub fn plan_switches(a: &Analysis, d: &Driver, min_gap: f64, drop_gap: f64) -> Vec<SwitchPoint> {
    let steps = downbeat_steps(a, d);
    let thr = DROP_FACTOR * median(steps.iter().map(|s| s.abs()).collect());
    let mut out = Vec::new();
    let mut last = 0.0;
    for (&t, &step) in a.downbeats.iter().zip(&steps) {
        if t >= a.duration {
            break;
        }
        let elapsed = t - last;
        let drop = thr > 0.0 && step > thr;
        let cut = if drop && elapsed >= drop_gap {
            Cut::Hard
        } else if elapsed >= min_gap {
            Cut::Soft
        } else {
            continue;
        };
        out.push(SwitchPoint { t, cut });
        last = t;
    }
    out
}
