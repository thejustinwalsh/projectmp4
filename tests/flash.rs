use projectmp4::render::flash::{
    count_flashes, region_luma, srgb_to_linear_lut, worst_second, FlashGuard,
};

#[path = "common/flash.rs"]
mod fx;
use fx::{cell_frame, lin, random_cells, solid, square_frame, square_luma, H, W};
use projectmp4::render::flash::window_side;

#[test]
fn a_full_frame_strobe_is_limited_to_the_threshold() {
    // 60 frames alternating near-black and near-white, the worst possible input.
    let mut g = FlashGuard::new(W, H, 60, 0.48);
    let mut lumas = Vec::new();
    for n in 0..60 {
        let mut f = solid(if n % 2 == 0 { 8 } else { 247 });
        g.limit(&mut f);
        lumas.push(region_luma(&f, W, H, 1, 1)[0]);
    }
    let fl = count_flashes(&lumas);
    let (worst, _) = worst_second(lumas.len(), &fl, 60);
    assert!(worst <= 3, "strobe still yields {worst} flashes in a second (limit 3)");
    // And the input really was a strobe: unguarded it is a flash per frame.
    let raw: Vec<f32> =
        (0..60).map(|n| region_luma(&solid(if n % 2 == 0 { 8 } else { 247 }), W, H, 1, 1)[0]).collect();
    assert!(worst_second(60, &count_flashes(&raw), 60).0 > 3, "control: the raw strobe must fail");
}

#[test]
fn a_calm_frame_passes_through_untouched() {
    // Two frames 0.5% apart in luminance must not be dimmed at all: the guard is
    // a limiter, not a filter. Byte-exact equality.
    let mut g = FlashGuard::new(W, H, 60, 0.48);
    let mut a = solid(120);
    g.limit(&mut a);
    let before = solid(121);
    let mut after = before.clone();
    let alpha = g.limit(&mut after);
    assert_eq!(alpha, 1.0, "calm frame was blended");
    assert_eq!(after, before, "calm frame was modified");
    assert_eq!(g.engaged_frames(), 0);
    assert_eq!(g.min_alpha(), 1.0);
}

#[test]
fn the_first_frame_passes_through_even_when_bright() {
    // Initialising the previous frame from black would fade frame 1 in from nothing.
    let mut g = FlashGuard::new(W, H, 60, 0.48);
    let before = solid(250);
    let mut f = before.clone();
    assert_eq!(g.limit(&mut f), 1.0);
    assert_eq!(f, before);
}

#[test]
fn a_strobe_confined_to_one_region_is_still_caught() {
    // THE POINT OF THE GRID. One cell (1/64 of the frame) alternates black/white,
    // two frames each, while the rest holds mid-grey. The frame mean moves by only
    // ~1/64 of the swing, so a frame-mean-only limiter lets the cell through.
    let (row, col) = (3, 5);
    let frame = |n: usize| cell_frame(row, col, if (n / 2) % 2 == 0 { 0 } else { 255 });
    let mut g = FlashGuard::new(W, H, 60, 0.48);
    let (mut cell, mut raw) = (Vec::new(), Vec::new());
    for n in 0..120 {
        raw.push(region_luma(&frame(n), W, H, 8, 8)[row * 8 + col]);
        let mut f = frame(n);
        g.limit(&mut f);
        cell.push(region_luma(&f, W, H, 8, 8)[row * 8 + col]);
    }
    assert!(worst_second(120, &count_flashes(&raw), 60).0 > 3, "control: the raw cell strobe must fail");
    let (worst, at) = worst_second(cell.len(), &count_flashes(&cell), 60);
    assert!(worst <= 3, "cell strobe still yields {worst} flashes in a second from frame {at} (limit 3)");
}

#[test]
fn the_guard_never_brightens_or_inverts() {
    // Output luminance per region must lie between the previous output and the
    // incoming frame, for 200 frames of pseudorandom input. No overshoot.
    let mut g = FlashGuard::new(W, H, 60, 0.48);
    let mut prev: Option<Vec<f32>> = None;
    // Solid cells round every pixel the same way, so allow one 8-bit step of output
    // quantisation (about 3.5e-3 of luminance at the bright end) and no more.
    let eps = 4e-3;
    for n in 0..200 {
        let input = random_cells(n);
        let want = region_luma(&input, W, H, 8, 8);
        let mut f = input.clone();
        g.limit(&mut f);
        let got = region_luma(&f, W, H, 8, 8);
        if let Some(p) = &prev {
            for r in 0..64 {
                let (lo, hi) = (p[r].min(want[r]) - eps, p[r].max(want[r]) + eps);
                assert!(
                    (lo..=hi).contains(&got[r]),
                    "frame {n} region {r}: out {} outside [{}, {}] (prev out {}, in {})",
                    got[r], lo, hi, p[r], want[r]
                );
            }
        }
        prev = Some(got);
    }
    assert!(g.engaged_frames() > 100, "random input should engage the guard constantly");
}

#[test]
fn flash_counting_matches_the_wcag_definition() {
    // Under 10%: not a flash.
    assert!(count_flashes(&[0.09, 0.0, 0.09]).is_empty(), "a 0.09 swing is under 10%");
    // 0.15 swing with a dark end: one flash.
    assert_eq!(count_flashes(&[0.0, 0.15, 0.0]).len(), 1);
    // Same pair with both ends above 0.80: not a flash.
    assert!(count_flashes(&[0.84, 0.99, 0.84]).is_empty(), "darker end >= 0.80 is exempt");
    // A monotonic ramp of total change 0.9: no opposing pair.
    let ramp: Vec<f32> = (0..=90).map(|i| i as f32 / 100.0).collect();
    assert!(count_flashes(&ramp).is_empty(), "a ramp has no opposing change");
    // Jitter inside a leg does not split it into sub-threshold legs.
    assert_eq!(count_flashes(&[0.0, 0.08, 0.079, 0.15, 0.0]).len(), 1);
    // A sustained oscillation is one flash per CYCLE (two legs), not one per leg:
    // 19 legs give 9 flashes, and worst_second bins them.
    let osc: Vec<f32> = (0..20).map(|i| if i % 2 == 0 { 0.0 } else { 0.2 }).collect();
    let fl = count_flashes(&osc);
    assert_eq!(fl, [2, 4, 6, 8, 10, 12, 14, 16, 18]);
    let four_legs = [0.0, 0.2, 0.0, 0.2, 0.0];
    assert_eq!(count_flashes(&four_legs).len(), 2, "4 legs are 2 cycles");
    assert_eq!(worst_second(20, &fl, 10), (5, 2), "flashes at 2,4,..,18: the window from 2 holds 2..=10");
    assert_eq!(worst_second(20, &[], 10), (0, 0));
}

#[test]
fn linearisation_round_trips_against_the_srgb_formula() {
    // The LUT must agree with the piecewise sRGB curve to 1e-6 at all 256 inputs,
    // including the 0.04045 knee. A plain 2.2 power law fails this.
    let lut = srgb_to_linear_lut();
    for i in 0..256usize {
        let c = i as f64 / 255.0;
        let want = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        assert!((lut[i] as f64 - want).abs() < 1e-6, "byte {i}: lut {} vs formula {want}", lut[i]);
        let power = c.powf(2.2);
        if i == 10 {
            assert!((power - want).abs() > 1e-4, "control: the 2.2 power law must differ at byte 10");
        }
    }
    assert_eq!(lut[0], 0.0);
    assert_eq!(lut[255], 1.0);
}

#[test]
fn the_guard_is_deterministic_and_frame_rate_independent_in_its_contract() {
    // Same frame sequence twice through two fresh guards gives byte-identical output.
    let run = || {
        let mut g = FlashGuard::new(W, H, 60, 0.48);
        (0..40)
            .flat_map(|n| {
                let mut f = random_cells(n);
                g.limit(&mut f);
                f
            })
            .collect::<Vec<u8>>()
    };
    assert!(run() == run(), "two fresh guards disagreed on the same input");
}

/// Long enough for a dozen 1 s windows; the guard runs unoptimised in tests.
const SECONDS: u32 = 6;
const FPS_LIST: [u32; 6] = [10, 24, 30, 60, 120, 240];

/// Full-frame square wave between two greys with `half` frames per half-period.
fn square_wave(fps: u32, rate: f32, half: u32, seconds: u32) -> (Vec<f32>, Vec<f32>) {
    let mut g = FlashGuard::new(W, H, fps, rate);
    let (mut raw, mut out) = (Vec::new(), Vec::new());
    for n in 0..fps * seconds {
        let v = if (n / half) % 2 == 0 { 8 } else { 247 };
        raw.push(region_luma(&solid(v), W, H, 1, 1)[0]);
        let mut f = solid(v);
        g.limit(&mut f);
        out.push(region_luma(&f, W, H, 1, 1)[0]);
    }
    (raw, out)
}

#[test]
fn the_flash_bound_holds_at_every_supported_frame_rate() {
    // The shortest dwell that still lets the signal travel a full 0.10 leg is
    // half = ceil(0.10 / delta) frames: the output is then a triangle wave whose
    // legs are just over the flash threshold, which is exactly the boundary the
    // guard must hold. Bound: rate / 0.20 = 2.4 per second at every fps, so a
    // window holds 3 at most; and the guard must not be a freeze (it flashes).
    for fps in FPS_LIST {
        let delta = 0.48 / fps as f32;
        // +1: the emitted bytes are a quantised view, so a leg of exactly 0.10 can read 0.099.
        let half = (0.10 / delta).ceil() as u32 + 1;
        let (_, out) = square_wave(fps, 0.48, half, SECONDS);
        let n = count_flashes(&out);
        let (worst, _) = worst_second(out.len(), &n, fps);
        assert!(worst <= 3, "fps {fps}: {worst} flashes in a second, bound is 2.4");
        let expect = (SECONDS * fps) as f32 / (2.0 * half as f32);
        assert!(n.len() as f32 >= expect * 0.8 - 1.0, "fps {fps}: {} flashes in {SECONDS} s, expected about {expect}: the guard is over-damping or frozen", n.len());
        // Strength: the wave a guard twice as loose (rate 0.96) would pass at its own
        // boundary must be flattened by this one, and must flash >3/s through the loose
        // guard, or this test would also pass with a delta twice as large. (At 10 fps a
        // wave that fast has a half-period of 2 frames and tops out at 2.5/s: no control.)
        let fast = (0.10 / (0.96 / fps as f32)).ceil() as u32;
        let (_, tight) = square_wave(fps, 0.48, fast, SECONDS);
        assert!(worst_second(tight.len(), &count_flashes(&tight), fps).0 <= 3, "fps {fps}: fast wave got through");
        if fps >= 24 {
            let (_, loose) = square_wave(fps, 0.96, fast, SECONDS);
            let (lw, _) = worst_second(loose.len(), &count_flashes(&loose), fps);
            assert!(lw > 3, "fps {fps}: control, rate 0.96 gave only {lw} flashes: the test is too weak");
        }
    }
}

#[test]
fn travel_per_second_never_exceeds_the_rate() {
    // The guarantee in plain terms, independent of the flash counter: the summed
    // absolute change of frame-mean luminance over any one second is <= rate.
    let rate = 0.48f32;
    for fps in FPS_LIST {
        for half in [1, fps, fps * 2] {
            let half = half.max(1);
            let (_, out) = square_wave(fps, rate, half, SECONDS);
            let d: Vec<f32> = out.windows(2).map(|p| (p[1] - p[0]).abs()).collect();
            let mut worst = 0f32;
            let mut sum: f32 = d.iter().take(fps as usize).sum();
            worst = worst.max(sum);
            for k in fps as usize..d.len() {
                sum += d[k] - d[k - fps as usize];
                worst = worst.max(sum);
            }
            // 1.05, not 1.01: the state's travel is exactly `rate`, but the emitted bytes are a
            // quantised view of it, and under a zigzag their rounding does not telescope.
            assert!(worst <= rate * 1.05, "fps {fps} half {half}: {worst} travelled in one second, rate {rate}");
            if half >= fps {
                assert!(worst >= rate * 0.9, "fps {fps} half {half}: only {worst} travelled; the guard is not tracking its limit");
            }
        }
    }
}

#[test]
fn a_bright_region_is_not_frozen_at_any_frame_rate() {
    // One 8-bit step near white exceeds delta at 60 fps already (above byte 235)
    // and at 240 fps from byte 78. State kept in bytes rounds those moves to
    // nothing and freezes the picture; float state accumulates them.
    for fps in [60u32, 240] {
        let delta = 0.48 / fps as f32;
        let travel = lin(200) - lin(40);
        let need = (travel / delta).ceil() as usize;
        let mut g = FlashGuard::new(W, H, fps, 0.48);
        let mut first = solid(200);
        g.limit(&mut first);
        let mut prev = region_luma(&first, W, H, 1, 1)[0];
        let mut arrived = None;
        for n in 1..=need + 5 {
            let mut f = solid(40);
            g.limit(&mut f);
            let l = region_luma(&f, W, H, 1, 1)[0];
            assert!(l <= prev + 1e-6, "fps {fps} frame {n}: luminance rose {prev} -> {l}");
            prev = l;
            if arrived.is_none() && f[0] <= 41 {
                arrived = Some(n);
            }
        }
        let n = arrived.unwrap_or_else(|| panic!("fps {fps}: never arrived (stuck at luminance {prev}), needed ~{need} frames"));
        assert!(n <= need + 3, "fps {fps}: arrived after {n} frames, expected about {need}");
        assert!(n + 3 >= need, "fps {fps}: arrived after {n} frames, faster than delta allows ({need})");
    }
}

#[test]
fn a_strobe_the_size_of_the_wcag_area_threshold_is_caught() {
    // The smallest region WCAG counts is a window of window_side() px (266 at 1080p).
    // Here a square of exactly that size is centred on a corner shared by four cells
    // of an 8x8 grid, so each cell holds a quarter of it: a per-cell limit lets the
    // square swing ~4x as fast as the frame mean allows, a sliding window does not.
    const FW: usize = 480;
    const FH: usize = 270;
    let size = window_side(FW, FH);
    assert!((60..=70).contains(&size), "window side {size}");
    let (x0, y0) = (180 - size / 2, 135 - size / 2); // 180 = 3 * 60 and 135 = 4 * 33.75: cell corners
    let run = |fps: u32, half: usize, frames: usize| {
        let mut g = FlashGuard::new(FW, FH, fps, 0.48);
        let (mut raw, mut out) = (Vec::new(), Vec::new());
        for n in 0..frames {
            let f = square_frame(FW, FH, x0, y0, size, if (n / half) % 2 == 0 { 0 } else { 255 });
            raw.push(square_luma(&f, FW, x0, y0, size));
            let mut f = f;
            g.limit(&mut f);
            out.push(square_luma(&f, FW, x0, y0, size));
        }
        (raw, out)
    };
    // The direct bound: the square's own mean never moves more than delta in one frame
    // (plus one 8-bit step of output quantisation). At 10 fps delta is large enough
    // for that step to be a small slice of it.
    let (raw, out) = run(10, 2, 40);
    let delta = 0.48 / 10.0;
    let jump = |v: &[f32]| v.windows(2).map(|p| (p[1] - p[0]).abs()).fold(0f32, f32::max);
    assert!(jump(&raw) > 0.5, "control: the raw square must swing hard, got {}", jump(&raw));
    assert!(jump(&out) <= delta * 1.2, "square mean moved {} in one frame, delta {delta}", jump(&out));
    // And at 60 fps, on a wave whose guarded legs stay under 0.10: no flashes at all,
    // where a quarter-cell limit would let legs of 0.22 through.
    let half = (0.05f32 / (0.48 / 60.0)).ceil() as usize;
    let (raw, out) = run(60, half, 180);
    assert!(worst_second(raw.len(), &count_flashes(&raw), 60).0 > 3, "control: the raw square strobe must fail");
    let (worst, at) = worst_second(out.len(), &count_flashes(&out), 60);
    assert!(worst <= 3, "square strobe still yields {worst} flashes in a second from frame {at}");
}

#[test]
fn resizing_keeps_limiting_against_the_previous_output() {
    let mut g = FlashGuard::new(W, H, 60, 0.48);
    let mut a = solid(10);
    g.limit(&mut a);
    g.resize(W * 2, H * 2);
    let mut b = vec![250u8; W * 2 * H * 2 * 4];
    let alpha = g.limit(&mut b);
    assert!(alpha < 0.05, "a full-scale jump right after a resize must still be limited, alpha {alpha}");
}

#[test]
fn the_flash_rate_ceiling_is_below_the_exact_threshold() {
    use projectmp4::render::plan::check_flash_rate;
    for ok in [0.001f32, 0.3, 0.48, 0.53] {
        assert!(check_flash_rate(ok).is_ok(), "{ok} should be accepted");
    }
    for bad in [0.0f32, 0.0005, 0.54, 0.60, 1.0, f32::NAN] {
        assert!(check_flash_rate(bad).is_err(), "{bad} should be rejected");
    }
    let msg = check_flash_rate(0.6).unwrap_err().to_string();
    assert!(msg.contains("0.60") && msg.contains("0.54") && msg.contains("10%"), "{msg}");
}
