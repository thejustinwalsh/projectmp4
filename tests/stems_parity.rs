#[path = "common/agentdeck.rs"]
mod agentdeck;
use projectmp4::audio::stems::*;

#[test]
fn span_weight_is_unity_at_song_bounds() {
    let source = 576_000usize;
    let starts: Vec<usize> = span_starts(source).collect();
    assert_eq!(starts, vec![0, 216_000, 432_000]);
    let len = |s: usize| STEM_SPAN_FRAMES.min(source - s);
    assert_eq!(span_weight(0, len(0), 0, source), 1.0);
    assert_eq!(span_weight(0, len(216_000), 216_000, source), 0.0);
    let last = *starts.last().unwrap();
    assert_eq!(span_weight(len(last) - 1, len(last), last, source), 1.0);

    let mut wsum = vec![0.0f64; source];
    for &s in &starts {
        for f in 0..len(s) {
            wsum[s + f] += span_weight(f, len(s), s, source);
        }
    }
    let bad = wsum.iter().position(|w| !(*w > 0.0));
    assert!(bad.is_none(), "zero weight sum at frame {bad:?}");
}

fn read_stereo(path: &std::path::Path) -> Vec<f32> {
    let b = std::fs::read(path).unwrap();
    assert!(b.len() >= 44 && &b[..4] == b"RIFF" && &b[36..40] == b"data", "{path:?} header");
    let u16le = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u32le = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    assert_eq!((u16le(20), u16le(22), u32le(24), u16le(34)), (3, 2, 48000, 32), "{path:?} format");
    assert_eq!(u32le(40) as usize + 44, b.len());
    b[44..].chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// Fail unless the opt-out is set; a silent skip would read as a pass.
fn skip_or_fail(what: String) {
    if std::env::var_os("PROJECTMP4_NO_PARITY_ORACLE").is_some() {
        eprintln!("SKIPPED by PROJECTMP4_NO_PARITY_ORACLE: {what}");
        return;
    }
    panic!(
        "{what}\nThis is the only check that the stems match agent-deck's, so it must not pass \
         by default when it cannot run. If you are deliberately running without it, set \
         PROJECTMP4_NO_PARITY_ORACLE=1."
    );
}

#[test]
#[ignore = "runs the htdemucs worker three times (~40 s)"]
fn stems_match_the_cached_bundle_bit_exactly() {
    let root = agentdeck::root();
    let source = root.join("tracks/14a2aeaf2cb59d4836e69bef8de4e45a9da2ee737be999f912ad9013ed4a752a.m4a");
    let bundle = root.join("stems/bundles/9fe3f71b0a5b39d816094e3dabe93f8c94ad3aa3567f2df3825236d756202b20");
    if !source.exists() || !bundle.exists() {
        return skip_or_fail(format!("parity oracle missing: {} or {}", source.display(), bundle.display()));
    }
    let rt = match Runtime::locate() {
        Ok(rt) => rt,
        Err(e) => return skip_or_fail(format!("stems runtime package unavailable: {e}")),
    };
    let scratch = std::env::temp_dir().join(format!("projectmp4-stems-parity-{}", std::process::id()));
    let stems = separate(&rt, &source, &scratch).unwrap();
    let _ = std::fs::remove_dir_all(&scratch);
    assert_eq!(stems.frames, 576_000);

    for (role, mine) in [("bass", &stems.bass), ("drums", &stems.drums), ("other", &stems.other), ("vocals", &stems.vocals)] {
        let want = read_stereo(&bundle.join(format!("{role}.wav")));
        assert_eq!(mine.len(), want.len(), "{role} length");
        let max_abs_diff = mine.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        eprintln!("{role}: max_abs_diff = {max_abs_diff:e}");
        assert_eq!(max_abs_diff, 0.0, "{role} is not bit-exact");
    }
}
