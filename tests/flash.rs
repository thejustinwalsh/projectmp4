use projectmp4::render::flash::{
    count_flashes, region_luma, srgb_to_linear_lut, worst_second, FlashGuard,
};

#[path = "common/flash.rs"]
mod fx;
use fx::{cell_frame, random_cells, solid, square_frame, square_luma, H, W};

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
    // A sustained oscillation reports every completed pair, and worst_second bins them.
    let osc: Vec<f32> = (0..20).map(|i| if i % 2 == 0 { 0.0 } else { 0.2 }).collect();
    let fl = count_flashes(&osc);
    assert_eq!(fl.len(), 18);
    assert_eq!(worst_second(20, &fl, 10), (10, 2), "flashes at 2..=19, so the window from 2 holds 10");
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

#[test]
fn the_flash_bound_holds_at_every_supported_frame_rate() {
    // The guarantee is fps-independent: rate/0.20 flashes per second, whatever the fps.
    // A full-swing square wave of half-period 0.1 s (5 Hz) is the case a per-frame limit
    // gets wrong: the guard turns it into a triangle wave whose amplitude is rate * 0.1 s
    // = 0.048 at the right rate, but fps * delta * 0.1 s at a per-frame delta fixed for 60 fps,
    // which is 4x as large at 240 fps.
    for fps in [10u32, 24, 30, 60, 120, 240] {
        let half = (fps / 10).max(1);
        let mut g = FlashGuard::new(W, H, fps, 0.48);
        let (mut raw, mut lumas) = (Vec::new(), Vec::new());
        for n in 0..fps * 3 {
            let v = if (n / half) % 2 == 0 { 8 } else { 247 };
            raw.push(region_luma(&solid(v), W, H, 1, 1)[0]);
            let mut f = solid(v);
            g.limit(&mut f);
            lumas.push(region_luma(&f, W, H, 1, 1)[0]);
        }
        assert!(worst_second(raw.len(), &count_flashes(&raw), fps).0 > 3, "fps {fps}: control, the raw wave must fail");
        let (worst, _) = worst_second(lumas.len(), &count_flashes(&lumas), fps);
        assert!(worst <= 2, "fps {fps}: {worst} flashes in a second, bound is 2.4");
    }
}

#[test]
fn a_strobe_the_size_of_the_wcag_area_threshold_is_caught() {
    // 266x266 px is 25% of a 10-degree field at ~3 picture heights: the smallest
    // region WCAG counts. A 300x300 square at 1920x1080 is 1/4 scale here
    // (75x75 on 480x270), spanning several 8x8 cells and not aligned to them.
    const FW: usize = 480;
    const FH: usize = 270;
    let (x0, y0, size) = (150, 80, 75);
    // 6 frames dark, 6 light: slow enough that a per-frame slew limit alone is not enough.
    let frame = |n: usize| square_frame(FW, FH, x0, y0, size, if (n / 6) % 2 == 0 { 0 } else { 255 });
    let mut g = FlashGuard::new(FW, FH, 60, 0.48);
    let (mut raw, mut out) = (Vec::new(), Vec::new());
    for n in 0..120 {
        raw.push(square_luma(&frame(n), FW, x0, y0, size));
        let mut f = frame(n);
        g.limit(&mut f);
        out.push(square_luma(&f, FW, x0, y0, size));
    }
    assert!(worst_second(120, &count_flashes(&raw), 60).0 > 3, "control: the raw square strobe must fail");
    let (worst, at) = worst_second(120, &count_flashes(&out), 60);
    assert!(worst <= 2, "square strobe still yields {worst} flashes in a second from frame {at} (bound 2.4)");
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
