//! Photosensitive-seizure safety: WCAG 2.3.1 flash measurement and a limiter
//! that makes an over-threshold render unrepresentable.
//!
//! The limiter is an adaptive crossfade with the previous *output* frame, in
//! linear light. One global blend factor `alpha` is chosen so that no region's
//! mean luminance moves more than its allowance in one frame; because blending
//! is linear in every region, one scalar satisfies all of them at once. A calm
//! frame gets `alpha == 1` and is left byte-for-byte alone.

use std::sync::OnceLock;

/// Each change of a flash is at least this fraction of maximum relative luminance.
pub const FLASH_DELTA: f32 = 0.10;
/// A flash needs the darker state below this relative luminance.
pub const FLASH_DARK: f32 = 0.80;
/// More than this many flashes in any one-second window fails WCAG 2.3.1.
pub const FLASH_LIMIT_PER_SECOND: usize = 3;
/// Default maximum travel of relative luminance per second in any region
/// (0.008 per frame at 60 fps). The bound it gives is `rate / 0.20` = 2.4
/// flashes per second at every frame rate.
pub const DEFAULT_RATE: f32 = 0.48;
/// A rate of exactly this permits exactly 3.0 flashes per second, the
/// threshold rather than under it, so the rate must be strictly below it.
pub const MAX_RATE: f32 = 0.60;

/// The frame is divided into GRID x GRID cells, each constrained separately.
/// At 1920x1080 a cell is 240x135 = 32,400 px^2. WCAG's area condition is 25%
/// of a 10-degree field: at about 3 picture heights that is ~600 px across,
/// ~283,000 px^2, a quarter of it ~70,700 px^2 (about 266x266). A 4x4 grid
/// (480x270 = 129,600 px^2) was nearly twice that, so one cell could flash
/// alone and fail; 8x8 is comfortably under.
const GRID: usize = 8;
/// Every cell carries the same per-frame delta as the frame mean. A mean of
/// cells each within +/- delta is itself within +/- delta, so any union of
/// whole cells, and so any region that size or larger, inherits the bound.
/// (A region not aligned to the grid can exceed it by at most the ratio of
/// the cell it sits in to the part of that cell it covers.) The frame-mean
/// constraint is therefore implied; it is kept because it is nearly free.
const CELL_ALLOWANCE: f32 = 1.0;
const ENC_STEPS: usize = 4096;

fn srgb_decode(b: u8) -> f32 {
    let c = b as f64 / 255.0;
    (if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }) as f32
}

fn srgb_encode(l: f64) -> u8 {
    let c = if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
    (c * 255.0).round().clamp(0.0, 255.0) as u8
}

/// sRGB byte -> linear light, by the piecewise IEC 61966-2-1 curve.
pub fn srgb_to_linear_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0f32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            *v = srgb_decode(i as u8);
        }
        t
    })
}

fn encode_lut() -> &'static [u8; ENC_STEPS] {
    static LUT: OnceLock<[u8; ENC_STEPS]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0u8; ENC_STEPS];
        for (i, v) in t.iter_mut().enumerate() {
            *v = srgb_encode(i as f64 / (ENC_STEPS - 1) as f64);
        }
        t
    })
}

/// Per-cell sums of relative luminance and pixel counts, row-major.
fn cell_sums(px: &[u8], w: usize, h: usize, cols: usize, rows: usize) -> (Vec<f64>, Vec<u64>) {
    assert!(cols > 0 && rows > 0 && w >= cols && h >= rows, "grid {cols}x{rows} does not fit {w}x{h}");
    assert!(px.len() >= w * h * 4, "buffer holds {} bytes, {w}x{h} RGBA needs {}", px.len(), w * h * 4);
    let lut = srgb_to_linear_lut();
    let mut sums = vec![0f64; cols * rows];
    let mut counts = vec![0u64; cols * rows];
    for y in 0..h {
        let r = (y * rows / h).min(rows - 1);
        // Row-in-cell edges use the same integer split for every row.
        let row = &px[y * w * 4..(y + 1) * w * 4];
        for c in 0..cols {
            let (x0, x1) = (c * w / cols, (c + 1) * w / cols);
            let mut s = 0f32;
            for p in row[x0 * 4..x1 * 4].chunks_exact(4) {
                s += 0.2126 * lut[p[0] as usize] + 0.7152 * lut[p[1] as usize] + 0.0722 * lut[p[2] as usize];
            }
            sums[r * cols + c] += s as f64;
            counts[r * cols + c] += (x1 - x0) as u64;
        }
    }
    (sums, counts)
}

/// Mean *relative* (linear-light) luminance per grid cell, row-major, from
/// RGBA bytes. Rec.709 weights on linearised channels.
pub fn region_luma(px: &[u8], w: usize, h: usize, cols: usize, rows: usize) -> Vec<f32> {
    let (s, n) = cell_sums(px, w, h, cols, rows);
    s.iter().zip(&n).map(|(s, n)| (s / *n as f64) as f32).collect()
}

/// Indices of WCAG flashes in one luminance series.
///
/// A flash is a pair of opposing changes in relative luminance, each of at
/// least [`FLASH_DELTA`], where the darker state is below [`FLASH_DARK`].
/// Turning points are found with a hysteresis of `FLASH_DELTA`, so a leg with
/// jitter in it is still one leg. Each consecutive triple of turning points
/// `(a, b, c)` that is a flash is reported at the index of `c`, the frame that
/// completes it. Overlapping triples count separately, which is the stricter
/// reading: a sustained oscillation of N legs yields N-1.
pub fn count_flashes(series: &[f32]) -> Vec<usize> {
    let t = FLASH_DELTA - 1e-6;
    let Some(&first) = series.first() else { return Vec::new() };
    // (index, value) of turning points.
    let mut ext: Vec<(usize, f32)> = Vec::new();
    let (mut lo, mut hi) = ((0usize, first), (0usize, first));
    let mut dir = 0i8;
    let mut cand = (0usize, first);
    for (i, &v) in series.iter().enumerate().skip(1) {
        match dir {
            0 => {
                if v < lo.1 {
                    lo = (i, v);
                }
                if v > hi.1 {
                    hi = (i, v);
                }
                if v - lo.1 >= t && lo.0 < i {
                    ext.push(lo);
                    dir = 1;
                    cand = (i, v);
                } else if hi.1 - v >= t && hi.0 < i {
                    ext.push(hi);
                    dir = -1;
                    cand = (i, v);
                }
            }
            1 => {
                if v > cand.1 {
                    cand = (i, v);
                } else if cand.1 - v >= t {
                    ext.push(cand);
                    dir = -1;
                    cand = (i, v);
                }
            }
            _ => {
                if v < cand.1 {
                    cand = (i, v);
                } else if v - cand.1 >= t {
                    ext.push(cand);
                    dir = 1;
                    cand = (i, v);
                }
            }
        }
    }
    if dir != 0 {
        ext.push(cand);
    }
    let mut out = Vec::new();
    for w in ext.windows(3) {
        let (a, b, c) = (w[0].1, w[1].1, w[2].1);
        let opposing = (b - a) * (c - b) < 0.0;
        if opposing && (b - a).abs() >= t && (c - b).abs() >= t && a.min(b).min(c) < FLASH_DARK {
            out.push(w[2].0);
        }
    }
    out
}

/// `(count, start_index)` of the densest one-second window. A window starting
/// at `s` holds flashes at indices `s..s + fps`. `(0, 0)` when there are none.
pub fn worst_second(series_len: usize, flashes: &[usize], fps: u32) -> (usize, usize) {
    let fps = fps as usize;
    let f: Vec<usize> = flashes.iter().copied().filter(|&i| i < series_len).collect();
    let mut best = (0usize, 0usize);
    let mut hi = 0;
    for lo in 0..f.len() {
        // Window starting at flash `lo`.
        if hi < lo {
            hi = lo;
        }
        while hi < f.len() && f[hi] < f[lo] + fps {
            hi += 1;
        }
        if hi - lo > best.0 {
            best = (hi - lo, f[lo]);
        }
    }
    best
}

/// How many one-second windows (every start `0..=len - fps`) hold more than
/// [`FLASH_LIMIT_PER_SECOND`] flashes, and how many windows there are.
pub fn windows_over_limit(series_len: usize, flashes: &[usize], fps: u32) -> (usize, usize) {
    let fps = fps as usize;
    if series_len < fps {
        return (0, 0);
    }
    let total = series_len - fps + 1;
    let mut over = 0;
    for s in 0..total {
        let a = flashes.partition_point(|&i| i < s);
        let b = flashes.partition_point(|&i| i < s + fps);
        if b - a > FLASH_LIMIT_PER_SECOND {
            over += 1;
        }
    }
    (over, total)
}

/// Limits luminance change so output stays under the photosensitive-seizure
/// threshold. See the module docs.
///
/// The bound. Luminance in any region can travel at most `rate` per second
/// (`delta = rate / fps` per frame). One WCAG flash consumes at least 0.20 of
/// travel (two legs of >= 0.10), so flashes per second <= `rate / 0.20`,
/// independent of fps. At `rate = 0.48` that is 2.4. A rate of 0.60 gives
/// exactly 3.0, the threshold, which is why it is rejected.
pub struct FlashGuard {
    w: usize,
    h: usize,
    /// Per-frame delta: `rate / fps`.
    limit: f32,
    /// The previous output frame; empty before the first frame.
    prev: Vec<u8>,
    prev_sums: Vec<f64>,
    counts: Vec<u64>,
    engaged: u64,
    min_alpha: f32,
}

impl FlashGuard {
    /// `rate_per_second` is the most relative luminance any region may travel
    /// in one second; the per-frame delta is `rate_per_second / fps`.
    pub fn new(w: usize, h: usize, fps: u32, rate_per_second: f32) -> FlashGuard {
        assert!(w >= GRID && h >= GRID, "frame {w}x{h} is smaller than the {GRID}x{GRID} grid");
        assert!(fps > 0, "fps must be positive");
        assert!(rate_per_second > 0.0 && rate_per_second.is_finite(), "flash rate must be positive, got {rate_per_second}");
        let limit = rate_per_second / fps as f32;
        FlashGuard { w, h, limit, prev: Vec::new(), prev_sums: Vec::new(), counts: Vec::new(), engaged: 0, min_alpha: 1.0 }
    }

    /// Change the frame size (a resized window). The previous output is
    /// resampled, so the next frame is still limited against it.
    pub fn resize(&mut self, w: usize, h: usize) {
        assert!(w >= GRID && h >= GRID, "frame {w}x{h} is smaller than the {GRID}x{GRID} grid");
        if (w, h) == (self.w, self.h) {
            return;
        }
        if !self.prev.is_empty() {
            let mut n = vec![0u8; w * h * 4];
            for y in 0..h {
                let sy = y * self.h / h;
                for x in 0..w {
                    let sx = x * self.w / w;
                    n[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&self.prev[(sy * self.w + sx) * 4..(sy * self.w + sx) * 4 + 4]);
                }
            }
            let (sums, counts) = cell_sums(&n, w, h, GRID, GRID);
            self.prev = n;
            self.prev_sums = sums;
            self.counts = counts;
        }
        self.w = w;
        self.h = h;
    }

    /// Limit `px` (RGBA, `w*h*4`) against the previous output in place and
    /// return the blend factor applied: exactly `1.0` when untouched. The
    /// first frame passes through; there is nothing to flash against.
    pub fn limit(&mut self, px: &mut [u8]) -> f32 {
        let (w, h) = (self.w, self.h);
        assert_eq!(px.len(), w * h * 4, "frame is {} bytes, expected {}", px.len(), w * h * 4);
        let (sums, counts) = cell_sums(px, w, h, GRID, GRID);
        if self.prev.is_empty() {
            self.prev = px.to_vec();
            self.prev_sums = sums;
            self.counts = counts;
            return 1.0;
        }
        let total: u64 = self.counts.iter().sum();
        let mean = |s: &[f64]| s.iter().sum::<f64>() / total as f64;
        let mut alpha = 1.0f64;
        let mut bound = |delta: f64, change: f64| {
            if change > delta {
                alpha = alpha.min(delta / change);
            }
        };
        bound(self.limit as f64, (mean(&sums) - mean(&self.prev_sums)).abs());
        for i in 0..sums.len() {
            let n = self.counts[i] as f64;
            bound((self.limit * CELL_ALLOWANCE) as f64, (sums[i] / n - self.prev_sums[i] / n).abs());
        }
        if alpha >= 1.0 {
            self.prev.copy_from_slice(px);
            self.prev_sums = sums;
            return 1.0;
        }
        let a = alpha as f32;
        let dec = srgb_to_linear_lut();
        let enc = encode_lut();
        let top = (ENC_STEPS - 1) as f32;
        for (o, p) in px.chunks_exact_mut(4).zip(self.prev.chunks_exact(4)) {
            for c in 0..3 {
                let l = a * dec[o[c] as usize] + (1.0 - a) * dec[p[c] as usize];
                o[c] = enc[(l * top + 0.5) as usize];
            }
        }
        self.prev.copy_from_slice(px);
        // Recomputed from the bytes actually emitted, so quantisation never accumulates.
        self.prev_sums = cell_sums(px, w, h, GRID, GRID).0;
        self.engaged += 1;
        self.min_alpha = self.min_alpha.min(a);
        a
    }

    /// Frames on which the guard changed anything.
    pub fn engaged_frames(&self) -> u64 {
        self.engaged
    }

    /// The smallest alpha applied so far (1.0 if the guard never engaged).
    pub fn min_alpha(&self) -> f32 {
        self.min_alpha
    }
}
