//! Section boundaries and labels from the model's logits (`ssa_postprocess` in the reference).

use crate::ffi::{Error, Result};

pub const LABELS: [&str; 10] = ["start", "end", "intro", "outro", "break", "bridge", "inst", "solo", "verse", "chorus"];

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub start: f64,
    pub end: f64,
    pub label: u32,
    pub confidence: f32,
}

/// Non-maximum-suppression half-width: filter size 4*24+1 = 97.
const NMS_HALF: usize = 48;
/// Dominant-peak window: 12 s at 100 fps.
const WINDOW: usize = 1200;
const FPS: f64 = 100.0;

/// `logits` is `[frames, 11]` as returned by `predict`.
///
/// DELIBERATE float/double mix, as the reference: steps 1-3 are f32 throughout (the prefix
/// sum is an f32 running sum, the window divisor is always 1200 even at the edges, peaks are
/// compared by exact f32 equality so ties keep every tied frame); step 4 takes f32
/// exponentials, a double normaliser, and double accumulation.
pub fn postprocess(logits: &[f32], frames: usize) -> Result<Vec<Section>> {
    if frames == 0 {
        return Err(Error::msg("postprocess: frames is 0"));
    }
    if logits.len() != frames * 11 {
        return Err(Error::msg(format!("postprocess: {} logits, expected {frames}*11", logits.len())));
    }
    if logits.iter().any(|v| !v.is_finite()) {
        return Err(Error::msg("postprocess: non-finite logit"));
    }

    // Step 1: boundary probability from the section logit (sign-split sigmoid).
    let prob: Vec<f32> = (0..frames)
        .map(|i| {
            let a = logits[i * 11];
            if a >= 0.0 {
                1.0 / (1.0 + (-a).exp())
            } else {
                a.exp() / (1.0 + a.exp())
            }
        })
        .collect();

    // Step 2: keep a frame only if it is the maximum within +-48 frames; f32 prefix sum.
    let mut local = vec![0f32; frames];
    let mut prefix = vec![0f32; frames + 1];
    for i in 0..frames {
        let mut best = 0f32;
        for &p in &prob[i.saturating_sub(NMS_HALF)..frames.min(i + NMS_HALF + 1)] {
            best = best.max(p);
        }
        if prob[i] == best {
            local[i] = prob[i];
        }
        prefix[i + 1] = prefix[i] + local[i];
    }

    // Step 3: a peak survives if it is the largest within +-12 s and exceeds the mean
    // strength of its surroundings. Frame 0 is never a candidate; it is always a boundary.
    let mut boundaries = vec![0usize];
    for i in 1..frames {
        if local[i] <= 0.0 {
            continue;
        }
        let lo = i.saturating_sub(WINDOW);
        let hi = frames.min(i + WINDOW + 1);
        let mut best = 0f32;
        for &l in &local[lo..hi] {
            best = best.max(l);
        }
        let past = (prefix[i] - prefix[lo]) / WINDOW as f32;
        let future = (prefix[hi] - prefix[i + 1]) / WINDOW as f32;
        if local[i] == best && local[i] - (past + future) * 0.5f32 > 0.0 {
            boundaries.push(i);
        }
    }
    boundaries.push(frames);

    // Step 4: label = argmax of the summed softmax over the 10 class logits (column 0 excluded).
    let mut out = Vec::with_capacity(boundaries.len() - 1);
    for b in boundaries.windows(2) {
        let (begin, end) = (b[0], b[1]);
        let mut sums = [0f64; 10];
        for i in begin..end {
            let row = &logits[i * 11 + 1..i * 11 + 11];
            let mut maximum = f32::NEG_INFINITY;
            for &v in row {
                maximum = maximum.max(v);
            }
            let mut total = 0f64;
            let mut p = [0f32; 10];
            for c in 0..10 {
                p[c] = (row[c] - maximum).exp();
                total += p[c] as f64;
            }
            for c in 0..10 {
                sums[c] += p[c] as f64 / total;
            }
        }
        // First maximum wins on ties, like std::max_element.
        let mut label = 0;
        for c in 1..10 {
            if sums[c] > sums[label] {
                label = c;
            }
        }
        out.push(Section {
            start: begin as f64 / FPS,
            end: end as f64 / FPS,
            label: label as u32,
            confidence: (sums[label] / (end - begin) as f64) as f32,
        });
    }
    Ok(out)
}
