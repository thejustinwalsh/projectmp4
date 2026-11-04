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

/// Writes a request -> bundle cache under `root`, with the bundle at `bundle`
/// and the request naming `job`. `declared` is each stem's manifest `frames`.
#[allow(clippy::too_many_arguments)]
fn build_cache(
    root: &Path,
    bundle: &Path,
    job: &str,
    drums: &[f32],
    bass: &[f32],
    completion: &str,
    manifest_sha: &str,
    declared: [usize; 2],
) {
    std::fs::create_dir_all(bundle).unwrap();
    std::fs::create_dir_all(root.join("stems/requests")).unwrap();
    write_wav(&bundle.join("drums.wav"), drums);
    write_wav(&bundle.join("bass.wav"), bass);
    let stem = |role: &str, frames: usize| {
        serde_json::json!({"role": role, "sampleRate": 48000, "channels": 2, "frames": frames,
            "path": bundle.join(format!("{role}.wav")).to_str().unwrap()})
    };
    let manifest = serde_json::json!({"schema": 1, "completion": completion,
        "sourceSha256": manifest_sha,
        "stems": [stem("drums", declared[0]), stem("bass", declared[1])]});
    std::fs::write(bundle.join("manifest.json"), manifest.to_string()).unwrap();
    std::fs::write(
        root.join("stems/requests").join(format!("{SHA}.json")),
        serde_json::json!({"track": {"sourceSha256": SHA}, "jobId": job}).to_string(),
    )
    .unwrap();
}

/// Synthetic cache root with manifest `frames` declared to match each stem.
/// `completion` and `manifest_sha` let tests break one guard at a time.
fn synthetic_cache(drums: &[f32], bass: &[f32], completion: &str, manifest_sha: &str) -> PathBuf {
    synthetic_cache_declared(drums, bass, completion, manifest_sha, [drums.len() / 2, bass.len() / 2])
}

/// As `synthetic_cache`, but the manifest's per-stem `frames` are given.
fn synthetic_cache_declared(
    drums: &[f32],
    bass: &[f32],
    completion: &str,
    manifest_sha: &str,
    declared: [usize; 2],
) -> PathBuf {
    let root = tmpdir();
    let bundle = root.join("stems/bundles").join(JOB);
    build_cache(&root, &bundle, JOB, drums, bass, completion, manifest_sha, declared);
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
    assert!(!d.samples().is_empty());
    let frames = d.frames();
    assert!(frames.abs_diff(RAVEZILLA_FRAMES) <= 2, "stem frames {frames}");
    let mix = Driver::load(&src, Some(DriverKind::Mix)).unwrap();
    assert!(d.samples() != mix.samples(), "stems driver is identical to the full mix");
}

#[test]
fn falls_back_to_full_mix_without_stems() {
    let d = Driver::load(&source_for("samples/track.mp3"), None).unwrap();
    assert_eq!(d.kind, DriverKind::Mix);
    assert!(!d.samples().is_empty());
}

#[test]
fn explicit_mix_request_overrides_available_stems() {
    let d = Driver::load(&source_for(RAVEZILLA), Some(DriverKind::Mix)).unwrap();
    assert_eq!(d.kind, DriverKind::Mix);
    assert!(!d.samples().is_empty());
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
    assert!(!d.samples().is_empty() && d.samples() == mix.samples());
}

#[test]
fn stems_are_summed_and_clamped() {
    let drums = [0.5, -0.5, 0.9, 0.0];
    let bass = [0.25, 0.25, 0.9, -1.0];
    let root = synthetic_cache(&drums, &bass, "complete", SHA);
    let d = Driver::load_with_cache_dir(&synthetic_source(), None, &root).unwrap();
    assert_eq!(d.kind, DriverKind::Stems);
    assert_eq!(d.samples(), &[0.75, -0.25, 1.0, -1.0]);
}

#[test]
fn drums_longer_than_bass_sums_over_the_shorter() {
    // Each stem declares its own true length, so only the stem lengths differ.
    let root = synthetic_cache(&[0.5; 6], &[0.25; 4], "complete", SHA);
    let d = Driver::load_with_cache_dir(&synthetic_source(), None, &root).unwrap();
    assert_eq!(d.kind, DriverKind::Stems);
    assert_eq!(d.samples(), &[0.75; 4]);
}

#[test]
fn bass_longer_than_drums_sums_over_the_shorter() {
    let root = synthetic_cache(&[0.5; 4], &[0.25; 6], "complete", SHA);
    let d = Driver::load_with_cache_dir(&synthetic_source(), None, &root).unwrap();
    assert_eq!(d.kind, DriverKind::Stems);
    assert_eq!(d.samples(), &[0.75; 4]);
    assert_eq!(d.frames(), 2);
}

#[test]
fn stem_shorter_than_its_declared_frames_is_a_miss() {
    // Control: the same files with bass declaring its true 2 frames load as
    // Stems (see `drums_longer_than_bass_sums_over_the_shorter`). The only
    // change here is bass declaring 4 frames while holding 2.
    let root = synthetic_cache_declared(&[0.5; 8], &[0.25; 4], "complete", SHA, [4, 4]);
    let d = Driver::load_with_cache_dir(&synthetic_source(), None, &root).unwrap();
    assert_eq!(d.kind, DriverKind::Mix, "truncated stem accepted as the whole track");
    assert_eq!(d.samples(), &[0.1; 6]);
}

#[test]
fn drums_shorter_than_declared_frames_is_a_miss() {
    let root = synthetic_cache_declared(&[0.5; 4], &[0.25; 8], "complete", SHA, [4, 4]);
    let d = Driver::load_with_cache_dir(&synthetic_source(), None, &root).unwrap();
    assert_eq!(d.kind, DriverKind::Mix);
}

#[test]
fn traversing_job_id_is_a_miss() {
    // Control (job id "job1", bundle under stems/bundles) loads Stems. Here the
    // request names "../../../etc" and a complete, valid bundle really exists
    // at that resolved path, so only the jobId guard stands between the two.
    let base = tmpdir();
    let root = base.join("cache");
    let bundle = base.join("etc");
    std::fs::create_dir_all(root.join("stems/bundles")).unwrap(); // so `..` resolves
    build_cache(&root, &bundle, "../../../etc", &[0.5; 4], &[0.25; 4], "complete", SHA, [2, 2]);
    assert!(root.join("stems/bundles/../../../etc/manifest.json").exists());
    let d = Driver::load_with_cache_dir(&synthetic_source(), None, &root).unwrap();
    assert_eq!(d.kind, DriverKind::Mix);
}

#[test]
fn window_is_exactly_min_frames_cap_for_every_input() {
    use projectmp4::audio::MAX_WINDOW_FRAMES;
    // 10 frames of 0.5 with default rate 48000: track is 10/48000 s long.
    let root = synthetic_cache(&[0.5; 20], &[0.25; 20], "complete", SHA);
    let d = Driver::load_with_cache_dir(&synthetic_source(), None, &root).unwrap();
    assert_eq!((d.frames(), d.rate), (10, 48_000));
    let sec = |f: f64| (f + 0.5) / 48_000.0; // lands mid-frame, immune to rounding
    // (label, t, frames, number of leading real samples expected, value)
    let cases: [(&str, f64, usize, usize); 9] = [
        ("inside", sec(2.0), 4, 4 * 2),
        ("straddling end", sec(8.0), 4, 2 * 2),
        ("wholly past", sec(50.0), 4, 0),
        ("negative", -1.0, 4, 0),
        ("nan", f64::NAN, 4, 0),
        ("infinity", f64::INFINITY, 4, 0),
        ("1e300", 1e300, 4, 0),
        ("zero frames", sec(2.0), 0, 0),
        ("usize::MAX frames", sec(2.0), usize::MAX, 8 * 2),
    ];
    for (label, t, frames, real) in cases {
        let w = d.window(t, frames);
        assert_eq!(w.len(), frames.min(MAX_WINDOW_FRAMES) * 2, "{label}: length");
        assert!(w[..real].iter().all(|s| *s == 0.75), "{label}: real part {:?}", &w[..real.min(8)]);
        assert!(w[real..].iter().all(|s| *s == 0.0), "{label}: tail not zero-padded");
    }
}

#[test]
fn incomplete_bundle_is_a_miss() {
    let root = synthetic_cache(&[0.5; 4], &[0.25; 4], "partial", SHA);
    let d = Driver::load_with_cache_dir(&synthetic_source(), None, &root).unwrap();
    assert_eq!(d.kind, DriverKind::Mix);
    assert_eq!(d.samples(), &[0.1; 6]);
}

#[test]
fn bundle_for_another_source_is_a_miss() {
    let root = synthetic_cache(&[0.5; 4], &[0.25; 4], "complete", "someone-else");
    let d = Driver::load_with_cache_dir(&synthetic_source(), None, &root).unwrap();
    assert_eq!(d.kind, DriverKind::Mix);
}
