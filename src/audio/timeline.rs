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
pub const HOP: usize = 1024;
/// Seconds of envelope averaged on each side of a downbeat to measure its step.
const STEP_WINDOW_S: f64 = 1.0;
/// A downbeat is a drop when its rise in low-band energy exceeds this many
/// times the median absolute step over all downbeats.
pub const DROP_FACTOR: f64 = 4.0;

/// Mean-square energy of the sub-200 Hz mono signal, one value per `HOP` frames.
pub fn low_band_envelope(d: &Driver) -> Vec<f64> {
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

/// Rise in low-band energy across each downbeat (after minus before), or
/// `None` where the track does not hold a full `STEP_WINDOW_S` of envelope on
/// both sides. Clamping instead would compare a 0.12 s "before" (six hops of
/// noise) against a 1 s "after", and that near-random step would feed the
/// median that calibrates every other downbeat.
fn downbeat_steps(downbeats: &[f64], d: &Driver) -> Vec<Option<f64>> {
    let env = low_band_envelope(d);
    let hop_s = HOP as f64 / d.rate as f64;
    let span = d.frames() as f64 / d.rate as f64;
    downbeats
        .iter()
        .map(|&t| {
            if t - STEP_WINDOW_S < 0.0 || t + STEP_WINDOW_S > span {
                return None;
            }
            Some(
                mean_over(&env, t, t + STEP_WINDOW_S, hop_s)
                    - mean_over(&env, t - STEP_WINDOW_S, t, hop_s),
            )
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

/// What the grid had to supply because the analysis left stretches without downbeats.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GridFill {
    /// Grid points added.
    pub added: usize,
    /// Seconds of the track that had no downbeat to switch on.
    pub secs: f64,
    /// The grid spacing the points were laid at: what [`notice`](Self::notice)
    /// reports, so the figure cannot disagree with the grid actually used.
    pub grid: f64,
}

/// The grid is worth mentioning once it carried this share of the track...
const NOTICE_FRACTION: f64 = 0.20;
/// ...or this many seconds, whichever is reached first. An outro or a short
/// breakdown is ordinary music; a line that fires on those gets skipped, and
/// then it is skipped on the 90 s intro it exists for.
const NOTICE_SECS: f64 = 60.0;

impl GridFill {
    /// What to tell the user, or `None` when the grid covered too little to matter.
    pub fn notice(&self, duration: f64) -> Option<String> {
        let enough = self.secs >= NOTICE_SECS || self.secs >= NOTICE_FRACTION * duration;
        (self.added > 0 && enough).then(|| {
            format!(
                "{:.0} s of this {duration:.0} s track ran on a fixed {} s interval instead of following the music: \
                 the analysis found no downbeats there (common in ambient intros, long breakdowns and outros)",
                self.secs,
                self.grid
            )
        })
    }
}

/// Fewer downbeats than this is not a schedule: one detected beat tells nothing
/// about where the next change belongs, and treating it as a schedule is worse
/// than having none.
const MIN_DOWNBEATS: usize = 2;

/// `downbeats` plus grid points (`grid` seconds apart) across every stretch of
/// `0..duration` longer than `2 * grid` that has none. The head counts from the
/// opening preset at `t = 0` and the tail runs to `duration`, so a 90 s ambient
/// intro, a single detected beat and a wholly beatless track are one case:
/// no downbeat is no reason for the picture to stop moving. A degenerate count
/// (see [`MIN_DOWNBEATS`]) is discarded and the whole track gets the grid, the
/// same as zero; so a track too short for the `2 * grid` rule still gets one.
pub fn fill_gaps(downbeats: &[f64], duration: f64, grid: f64) -> (Vec<f64>, GridFill) {
    let mut out: Vec<f64> = downbeats.iter().copied().filter(|t| t.is_finite() && *t >= 0.0 && *t < duration).collect();
    let mut fill = GridFill { grid, ..GridFill::default() };
    if !(grid.is_finite() && grid > 0.0 && duration.is_finite()) {
        return (out, fill);
    }
    let degenerate = out.len() < MIN_DOWNBEATS;
    if degenerate {
        out.clear();
    }
    let mut edges = vec![0.0];
    edges.extend_from_slice(&out);
    edges.push(duration);
    let last = edges.len() - 2;
    let mut extra = Vec::new();
    for (i, w) in edges.windows(2).enumerate() {
        let (prev, next) = (w[0], w[1]);
        if next - prev <= if degenerate { grid } else { 2.0 * grid } {
            continue;
        }
        // Stay clear of the real downbeat that ends the gap; the tail runs to the end.
        let limit = if i == last { duration } else { next - grid / 2.0 };
        let before = extra.len();
        let mut k = 1.0;
        while prev + k * grid < limit {
            extra.push(prev + k * grid);
            k += 1.0;
        }
        if extra.len() > before {
            fill.secs += next - prev;
        }
    }
    fill.added = extra.len();
    out.extend(extra);
    out.sort_by(|a, b| a.total_cmp(b));
    (out, fill)
}

/// Switch points: `min_gap` seconds apart at least, or `drop_gap` for a drop
/// (which is also marked `Hard`). Every point is a downbeat or, where the
/// analysis has none to offer, a grid point; all are `< a.duration`.
pub fn plan_switches(a: &Analysis, d: &Driver, min_gap: f64, drop_gap: f64) -> Vec<SwitchPoint> {
    plan_switches_reporting(a, d, min_gap, drop_gap).0
}

/// [`plan_switches`], and what the grid had to cover so the caller can say so.
pub fn plan_switches_reporting(a: &Analysis, d: &Driver, min_gap: f64, drop_gap: f64) -> (Vec<SwitchPoint>, GridFill) {
    let (points, fill) = fill_gaps(&a.downbeats, a.duration, min_gap);
    let steps = downbeat_steps(&points, d);
    // The scale is the typical *non-zero* step. Digital silence gives steps of
    // exactly 0.0, and when those are the majority a median over all of them is
    // 0.0 and no step could ever exceed it by a factor: the real drop would go
    // unmarked. A track with no non-zero step has nothing to call a drop.
    let thr = DROP_FACTOR * median(steps.iter().flatten().map(|s| s.abs()).filter(|s| *s > 0.0).collect());
    let mut out = Vec::new();
    let mut last = 0.0;
    for (&t, &step) in points.iter().zip(&steps) {
        if t >= a.duration {
            break;
        }
        let elapsed = t - last;
        let drop = step.is_some_and(|s| s > thr);
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
    (out, fill)
}
