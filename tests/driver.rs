use projectmp4::audio::{normalise, Driver, DriverKind, Source};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const RAVEZILLA: &str = "samples/ravezilla.m4a";
const RAVEZILLA_FRAMES: usize = 8_277_120;

fn tmpdir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "projectmp4-driver-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn source_for(p: &str) -> Source {
    normalise(Path::new(p), &tmpdir()).unwrap()
}

/// Canonical 48 kHz stereo f32 WAV from interleaved samples.
fn write_wav(path: &Path, pcm: &[f32]) {
    let data = (pcm.len() * 4) as u32;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&3u16.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&48_000u32.to_le_bytes());
    b.extend_from_slice(&(48_000u32 * 8).to_le_bytes());
    b.extend_from_slice(&8u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for s in pcm {
        b.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, b).unwrap();
}

const SHA: &str = "abc123";
const JOB: &str = "job1";

/// Synthetic cache root: request -> bundle with tiny drums/bass stems.
/// `completion` and `manifest_sha` let tests break one guard at a time.
fn synthetic_cache(drums: &[f32], bass: &[f32], completion: &str, manifest_sha: &str) -> PathBuf {
    let root = tmpdir();
    let bundle = root.join("stems/bundles").join(JOB);
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::create_dir_all(root.join("stems/requests")).unwrap();
    write_wav(&bundle.join("drums.wav"), drums);
    write_wav(&bundle.join("bass.wav"), bass);
    let stem = |role: &str| {
        serde_json::json!({"role": role, "sampleRate": 48000, "channels": 2,
            "path": bundle.join(format!("{role}.wav")).to_str().unwrap()})
    };
    let manifest = serde_json::json!({"schema": 1, "completion": completion,
        "sourceSha256": manifest_sha, "stems": [stem("drums"), stem("bass")]});
    std::fs::write(bundle.join("manifest.json"), manifest.to_string()).unwrap();
    std::fs::write(
        root.join("stems/requests").join(format!("{SHA}.json")),
        serde_json::json!({"track": {"sourceSha256": SHA}, "jobId": JOB}).to_string(),
    )
    .unwrap();
    root
}

/// A 3-frame mix of 0.1s, distinct from any stem sum used below.
fn synthetic_source() -> Source {
    let wav = tmpdir().join("mix.wav");
    write_wav(&wav, &[0.1; 6]);
    Source { wav, sha256: SHA.into(), duration: 0.0, title: None, artist: None }
}

#[test]
fn prefers_summed_drums_and_bass_stems_when_available() {
    let src = source_for(RAVEZILLA);
    let d = Driver::load(&src, None).unwrap();
    assert_eq!(d.kind, DriverKind::Stems);
    assert_eq!(d.rate, 48_000);
    assert!(!d.pcm.is_empty());
    let frames = d.pcm.len() / 2;
    assert!(frames.abs_diff(RAVEZILLA_FRAMES) <= 2, "stem frames {frames}");
    let mix = Driver::load(&src, Some(DriverKind::Mix)).unwrap();
    assert!(d.pcm != mix.pcm, "stems driver is identical to the full mix");
}

#[test]
fn falls_back_to_full_mix_without_stems() {
    let d = Driver::load(&source_for("samples/track.mp3"), None).unwrap();
    assert_eq!(d.kind, DriverKind::Mix);
    assert!(!d.pcm.is_empty());
}

#[test]
fn explicit_mix_request_overrides_available_stems() {
    let d = Driver::load(&source_for(RAVEZILLA), Some(DriverKind::Mix)).unwrap();
    assert_eq!(d.kind, DriverKind::Mix);
    assert!(!d.pcm.is_empty());
}

#[test]
fn window_past_end_of_track_is_silent_not_a_panic() {
    let d = Driver::load(&source_for("samples/track.mp3"), None).unwrap();
    let past = d.window(1e6, 800);
    assert_eq!(past.len(), 1600);
    assert!(past.iter().all(|s| *s == 0.0));
    let inside = d.window(30.0, 800);
    assert_eq!(inside.len(), 1600);
    assert!(inside.iter().any(|s| *s != 0.0), "window inside the track is silent");
}

#[test]
fn request_without_bundle_falls_back_to_mix() {
    let src = source_for("samples/track.mp3");
    let root = tmpdir();
    std::fs::create_dir_all(root.join("stems/requests")).unwrap();
    std::fs::write(
        root.join("stems/requests").join(format!("{}.json", src.sha256)),
        r#"{"jobId":"does-not-exist"}"#,
    )
    .unwrap();
    let d = Driver::load_with_cache_dir(&src, None, &root).unwrap();
    assert_eq!(d.kind, DriverKind::Mix);
    let mix = Driver::load_with_cache_dir(&src, Some(DriverKind::Mix), &root).unwrap();
    assert!(!d.pcm.is_empty() && d.pcm == mix.pcm);
}

#[test]
fn stems_are_summed_and_clamped() {
    let drums = [0.5, -0.5, 0.9, 0.0];
    let bass = [0.25, 0.25, 0.9, -1.0];
    let root = synthetic_cache(&drums, &bass, "complete", SHA);
    let d = Driver::load_with_cache_dir(&synthetic_source(), None, &root).unwrap();
    assert_eq!(d.kind, DriverKind::Stems);
    assert_eq!(d.pcm, vec![0.75, -0.25, 1.0, -1.0]);
}

#[test]
fn stems_of_unequal_length_sum_over_the_shorter() {
    let root = synthetic_cache(&[0.5; 6], &[0.25; 4], "complete", SHA);
    let d = Driver::load_with_cache_dir(&synthetic_source(), None, &root).unwrap();
    assert_eq!(d.kind, DriverKind::Stems);
    assert_eq!(d.pcm, vec![0.75; 4]);
}

#[test]
fn incomplete_bundle_is_a_miss() {
    let root = synthetic_cache(&[0.5; 4], &[0.25; 4], "partial", SHA);
    let d = Driver::load_with_cache_dir(&synthetic_source(), None, &root).unwrap();
    assert_eq!(d.kind, DriverKind::Mix);
    assert_eq!(d.pcm, vec![0.1; 6]);
}

#[test]
fn bundle_for_another_source_is_a_miss() {
    let root = synthetic_cache(&[0.5; 4], &[0.25; 4], "complete", "someone-else");
    let d = Driver::load_with_cache_dir(&synthetic_source(), None, &root).unwrap();
    assert_eq!(d.kind, DriverKind::Mix);
}
