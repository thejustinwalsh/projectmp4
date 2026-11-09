use projectmp4::encode::{video_bitrate_kbps, Target};

#[test]
fn a_short_track_keeps_the_default_because_capping_would_raise_the_bitrate() {
    // 90 s at Discord's 500 MB allows 40.6 Mb/s. The default encode cannot
    // exceed its 12 Mb/s ceiling, so it already fits with room to spare, and
    // capping at the budget would INFLATE the file. Must return None.
    assert_eq!(video_bitrate_kbps(Target::Discord, 90.0, 256).unwrap(), None);
}

#[test]
fn a_long_track_is_capped_to_fit_with_margin() {
    // 300 s at 500 MB leaves a 12010.7 kb/s budget, just above the 12000
    // ceiling, so the ceiling is what is asked for.
    let k = video_bitrate_kbps(Target::Discord, 300.0, 256).unwrap().unwrap();
    assert_eq!(k, 12_000, "got {k} kbps");
    let bytes = (k as f64 + 256.0) * 1000.0 * 300.0 / 8.0;
    assert!(bytes < 500_000_000.0, "{bytes} bytes does not fit 500 MB");
}

#[test]
fn the_boundary_is_where_capping_stops_inflating_the_file() {
    // CAP_CRF_AT = 15000 kbps of budget: 500e6*8*0.92/1000/(15000+256) = 241.2 s.
    // Under it the default is kept, because it is bounded at 12000 and so
    // cannot overrun; over it an explicit cap is asked for.
    assert_eq!(video_bitrate_kbps(Target::Discord, 241.0, 256).unwrap(), None);
    assert_eq!(video_bitrate_kbps(Target::Discord, 242.0, 256).unwrap(), Some(12_000));
    // The full 219.72 s sample now sits on the UNCAPPED side, and that is the
    // point of the ceiling: 219.72 s at 12000 + 256 kb/s is 337 MB, inside
    // Discord's 500 MB without a cap. Uncapped CRF 17 spent 67.8 Mb/s on this
    // content, which would have been 1.86 GB.
    assert_eq!(video_bitrate_kbps(Target::Discord, 219.72, 256).unwrap(), None);
    let bytes = (12_000.0 + 256.0) * 1000.0 * 219.72 / 8.0;
    assert!(bytes < 500e6, "{bytes} bytes does not fit 500 MB uncapped");
}

#[test]
fn a_track_too_long_to_look_acceptable_is_an_error_not_a_mush() {
    // 900 s at 500 MB gives ~3.83 Mb/s, under FLOOR_KBPS (6000).
    let e = video_bitrate_kbps(Target::Discord, 900.0, 256).unwrap_err();
    let m = e.0.to_lowercase();
    assert!(m.contains("fps") && m.contains("youtube"), "unhelpful message: {}", e.0);
}

#[test]
fn x_rejects_a_track_over_its_duration_cap_regardless_of_size() {
    // A DURATION wall: no bitrate fixes it. 180 s is Err though 512 MB is plenty (~26.7 Mb/s at 140 s).
    let e = video_bitrate_kbps(Target::X, 180.0, 256).unwrap_err();
    assert!(e.0.contains("140") && e.0.contains("--limit-seconds"), "unhelpful message: {}", e.0);
    // At the cap itself it is allowed, and 512 MB over 140 s is far above CRF cost.
    assert_eq!(video_bitrate_kbps(Target::X, 140.0, 256).unwrap(), None);
}

#[test]
fn youtube_and_none_never_cap() {
    for t in [Target::YouTube, Target::None] {
        assert_eq!(video_bitrate_kbps(t, 3600.0, 256).unwrap(), None);
    }
}

#[test]
fn a_zero_or_absurd_duration_cannot_panic_or_divide_by_zero() {
    for t in [Target::None, Target::Discord, Target::X, Target::YouTube] {
        for d in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(video_bitrate_kbps(t, d, 256).is_err(), "{t:?} {d}");
        }
    }
}

#[test]
fn the_bitrate_rounds_down() {
    // 350 s: 500e6*8*0.92/350/1000 - 256 = 10258.2857..: floor, not round.
    // Chosen past the 300.3 s point where the budget drops under the ceiling,
    // so the budget itself is the binding value and its fraction is visible.
    assert_eq!(video_bitrate_kbps(Target::Discord, 350.0, 256).unwrap(), Some(10_258));
}

#[test]
fn target_names_round_trip() {
    for t in [Target::None, Target::Discord, Target::X, Target::YouTube] {
        assert_eq!(Target::parse(t.name()).unwrap(), t);
    }
    assert!(Target::parse("tiktok").is_err());
}

#[test]
fn capping_never_asks_for_more_bitrate_than_the_default_would_spend() {
    // Across the window where the budget sits between the ceiling (12000) and
    // CAP_CRF_AT (15000) — 241.3 s to 300.2 s — a cap at the full budget would
    // ask for more than the default encode can even produce, inflating the file.
    for tenths in 2420..=3002 {
        let d = tenths as f64 / 10.0;
        let budget = 500e6 * 8.0 * 0.92 / d / 1000.0 - 256.0;
        let k = video_bitrate_kbps(Target::Discord, d, 256).unwrap().unwrap() as f64;
        assert!(k <= 12_000.0, "{d} s: {k} kbps exceeds the 12000 ceiling");
        assert!(k <= budget, "{d} s: {k} kbps exceeds the budget {budget}");
    }
    assert_eq!(video_bitrate_kbps(Target::Discord, 242.0, 256).unwrap(), Some(12_000));
    assert_eq!(video_bitrate_kbps(Target::Discord, 300.0, 256).unwrap(), Some(12_000));
    // Where the budget is already below the ceiling it is used as is.
    assert_eq!(video_bitrate_kbps(Target::Discord, 301.0, 256).unwrap(), Some(11_969));
    assert_eq!(video_bitrate_kbps(Target::Discord, 400.0, 256).unwrap(), Some(8_944));
}

// --- The --fast encoder sizes differently from libx264 ---

#[test]
fn the_fast_encoder_never_trusts_constant_quality_to_fit_a_capped_target() {
    use projectmp4::encode::video_bitrate_kbps_fast;
    // 176.87 s at Discord's 500 MB leaves 20.55 Mb/s, above CAP_CRF_AT, so the
    // libx264 sizing says "the default already fits" and caps nothing. The two
    // encoders do not cost the same, so the fast sizing does not inherit that
    // answer: it hands back the real budget, which is also the larger of the
    // two and so the better quality. The original defect here was the fast
    // default being `-q:v 65`, measured at 109 Mb/s, which wrote 2.4 GB against
    // a 500 MB cap; both the default and this sizing now bound it.
    assert_eq!(video_bitrate_kbps(Target::Discord, 176.87, 256).unwrap(), None);
    let k = video_bitrate_kbps_fast(Target::Discord, 176.87, 256).unwrap().expect("a capped target must be capped");
    assert_eq!(k, 20_550);
    // And it has to be a bitrate the cap can actually hold.
    let bytes = (k + 256) as f64 * 1000.0 * 176.87 / 8.0;
    assert!(bytes <= 500e6, "{bytes} bytes over a 500 MB cap");

    // The same hole on X: any clip short enough to be legal has a budget above
    // CAP_CRF_AT, so every single --fast --target x render took the shortcut.
    assert_eq!(video_bitrate_kbps(Target::X, 135.0, 256).unwrap(), None);
    let k = video_bitrate_kbps_fast(Target::X, 135.0, 256).unwrap().expect("a capped target must be capped");
    let bytes = (k + 256) as f64 * 1000.0 * 135.0 / 8.0;
    assert!(bytes <= 512e6, "{bytes} bytes over a 512 MB cap");
}

#[test]
fn the_fast_encoder_keeps_its_default_where_nothing_caps_it() {
    use projectmp4::encode::video_bitrate_kbps_fast;
    for t in [Target::None, Target::YouTube] {
        assert_eq!(video_bitrate_kbps_fast(t, 300.0, 256).unwrap(), None, "{t:?}");
    }
}

#[test]
fn the_fast_encoder_refuses_exactly_what_the_slow_one_refuses() {
    use projectmp4::encode::video_bitrate_kbps_fast;
    // Feasibility is a property of the target and the length, not the encoder.
    for t in [Target::None, Target::Discord, Target::X, Target::YouTube] {
        for d in [0.0, -1.0, f64::NAN, f64::INFINITY, 900.0, 180.0, 135.0, 60.0, 300.0] {
            assert_eq!(
                video_bitrate_kbps(t, d, 256).is_err(),
                video_bitrate_kbps_fast(t, d, 256).is_err(),
                "{t:?} {d}"
            );
        }
    }
}
