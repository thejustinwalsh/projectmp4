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
/// (0.008 per frame at 60 fps). Gives at most `rate / 0.20` = 2.4 flashes per
/// second at every frame rate, under the non-overlapping count of
/// [`count_flashes`].
pub const DEFAULT_RATE: f32 = 0.48;
/// Rates at or above this are rejected. 0.60 is the exact threshold (0.60 / 0.20
/// = 3.0 flashes per second); 0.54 leaves 10% headroom under it.
pub const MAX_RATE: f32 = 0.54;
/// The smallest region WCAG counts: 25% of a 10-degree visual field at about
/// 3 picture heights is ~70,700 px^2 at 1080p, a square of about 266 px. For
/// other frame heights the side scales with the height.
pub const WCAG_WINDOW_PX: usize = 266;

/// Side of the square window the guard bounds, for a `w`x`h` frame.
pub fn window_side(w: usize, h: usize) -> usize {
    ((WCAG_WINDOW_PX as f64 * h as f64 / 1080.0).round() as usize).clamp(1, w.min(h))
}

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
/// completes it. Triples do not overlap: a matched one is consumed and the
/// scan resumes from its end, so a sustained oscillation of N legs yields N/2
/// flashes, one per cycle.
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
    // Non-overlapping: a matched triple is consumed and the scan resumes from
    // its last turning point, so a zigzag of N legs yields N/2 flashes (one per
    // oscillation cycle), which is what "3 flashes per second" means.
    let mut k = 0;
    while k + 2 < ext.len() {
        let (a, b, c) = (ext[k].1, ext[k + 1].1, ext[k + 2].1);
        let opposing = (b - a) * (c - b) < 0.0;
        if opposing && (b - a).abs() >= t && (c - b).abs() >= t && a.min(b).min(c) < FLASH_DARK {
            out.push(ext[k + 2].0);
            k += 2;
        } else {
            k += 1;
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
/// Per frame, the guard blends the incoming frame into its previous output,
/// in linear light, with one scalar `alpha <= 1` chosen so that the mean
/// luminance of the whole frame and of EVERY square window of the WCAG area
/// size ([`window_side`], about 266 px at 1080p) moves by at most
/// `delta = rate / fps`. Blending is linear in every region, so one scalar
/// satisfies all windows. A summed-area table over the per-pixel luminance
/// change gives every window mean in O(1), so the scan is O(pixels). Any
/// region WCAG counts contains a full window, so it is bounded at any position
/// and alignment.
///
/// The bound. Luminance in any region travels at most `rate` per second. One
/// WCAG flash is a pair of opposing changes of at least 0.10 each, so it
/// consumes at least 0.20 of travel, and flashes per second <= `rate / 0.20`,
/// independent of fps: 2.4 at the default 0.48. This assumes (1) the
/// non-overlapping count of [`count_flashes`] (an overlapping count charges
/// each flash one leg, 0.10, and would double the bound), and (2) that the
/// state is held in float, so travel is exactly `rate` with no rounding bias.
///
/// The previous output is kept as `f32` linear RGB, never re-read from bytes.
/// The byte buffer handed back is a quantised view of that state. A delta
/// smaller than one 8-bit step therefore accumulates in float instead of
/// rounding to nothing, so the picture never freezes at any frame rate.
pub struct FlashGuard {
    w: usize,
    h: usize,
    /// Per-frame delta: `rate / fps`.
    delta: f32,
    win: usize,
    /// Previous output, linear RGB, `w*h*3`; empty before the first frame.
    prev: Vec<f32>,
    /// Summed-area table of the luminance change, `(w+1)*(h+1)`.
    sat: Vec<f64>,
    engaged: u64,
    min_alpha: f32,
}

impl FlashGuard {
    /// `rate_per_second` is the most relative luminance any region may travel
    /// in one second; the per-frame delta is `rate_per_second / fps`.
    pub fn new(w: usize, h: usize, fps: u32, rate_per_second: f32) -> FlashGuard {
        assert!(w >= 8 && h >= 8, "frame {w}x{h} is too small to guard");
        assert!(fps > 0, "fps must be positive");
        assert!(rate_per_second > 0.0 && rate_per_second.is_finite(), "flash rate must be positive, got {rate_per_second}");
        FlashGuard {
            w,
            h,
            delta: rate_per_second / fps as f32,
            win: window_side(w, h),
            prev: Vec::new(),
            sat: Vec::new(),
            engaged: 0,
            min_alpha: 1.0,
        }
    }

    /// Change the frame size (a resized window). The previous output is
    /// resampled, so the next frame is still limited against it.
    pub fn resize(&mut self, w: usize, h: usize) {
        assert!(w >= 8 && h >= 8, "frame {w}x{h} is too small to guard");
        if (w, h) == (self.w, self.h) {
            return;
        }
        if !self.prev.is_empty() {
            let mut n = vec![0f32; w * h * 3];
            for y in 0..h {
                let sy = y * self.h / h;
                for x in 0..w {
                    let sx = x * self.w / w;
                    n[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&self.prev[(sy * self.w + sx) * 3..(sy * self.w + sx) * 3 + 3]);
                }
            }
            self.prev = n;
        }
        self.w = w;
        self.h = h;
        self.win = window_side(w, h);
        self.sat = Vec::new();
    }

    /// Limit `px` (RGBA, `w*h*4`) against the previous output in place and
    /// return the blend factor applied: exactly `1.0` when untouched. The
    /// first frame passes through; there is nothing to flash against.
    pub fn limit(&mut self, px: &mut [u8]) -> f32 {
        let (w, h) = (self.w, self.h);
        assert_eq!(px.len(), w * h * 4, "frame is {} bytes, expected {}", px.len(), w * h * 4);
        let dec = srgb_to_linear_lut();
        if self.prev.is_empty() {
            self.prev = vec![0f32; w * h * 3];
            self.absorb(px);
            return 1.0;
        }
        let stride = w + 1;
        if self.sat.len() != stride * (h + 1) {
            self.sat = vec![0f64; stride * (h + 1)];
        }
        // Pass 1: summed-area table of the per-pixel luminance change.
        for y in 0..h {
            let row = &px[y * w * 4..(y + 1) * w * 4];
            let prow = &self.prev[y * w * 3..(y + 1) * w * 3];
            let mut run = 0f64;
            for x in 0..w {
                let (i, j) = (x * 4, x * 3);
                let l_in = 0.2126 * dec[row[i] as usize] + 0.7152 * dec[row[i + 1] as usize] + 0.0722 * dec[row[i + 2] as usize];
                let l_pv = 0.2126 * prow[j] + 0.7152 * prow[j + 1] + 0.0722 * prow[j + 2];
                run += (l_in - l_pv) as f64;
                self.sat[(y + 1) * stride + x + 1] = self.sat[y * stride + x + 1] + run;
            }
        }
        // The whole frame, and the largest window mean anywhere.
        let mut worst = (self.sat[h * stride + w] / (w * h) as f64).abs();
        let (sx, sy) = (self.win, self.win);
        let (stride_x, stride_y) = (1, 1);
        let area = (sx * sy) as f64;
        let mut y = 0;
        while y + sy <= h {
            let (a, b) = (y * stride, (y + sy) * stride);
            let mut x = 0;
            while x + sx <= w {
                let v = (self.sat[b + x + sx] - self.sat[a + x + sx] - self.sat[b + x] + self.sat[a + x]).abs() / area;
                if v > worst {
                    worst = v;
                }
                x += stride_x;
            }
            y += stride_y;
        }
        let delta = self.delta as f64;
        if worst <= delta {
            self.absorb(px);
            return 1.0;
        }
        let a = (delta / worst) as f32;
        let enc = encode_lut();
        let top = (ENC_STEPS - 1) as f32;
        for (o, p) in px.chunks_exact_mut(4).zip(self.prev.chunks_exact_mut(3)) {
            for c in 0..3 {
                let st = p[c] + a * (dec[o[c] as usize] - p[c]);
                p[c] = st;
                o[c] = enc[((st * top + 0.5) as usize).min(ENC_STEPS - 1)];
            }
        }
        self.engaged += 1;
        self.min_alpha = self.min_alpha.min(a);
        a
    }

    /// Take `px` as the new state, untouched.
    fn absorb(&mut self, px: &[u8]) {
        let dec = srgb_to_linear_lut();
        for (o, p) in px.chunks_exact(4).zip(self.prev.chunks_exact_mut(3)) {
            for c in 0..3 {
                p[c] = dec[o[c] as usize];
            }
        }
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
