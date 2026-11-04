use projectmp4::audio::{normalise, normalise_with_ffmpeg};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

fn tmpdir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "projectmp4-normalise-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wav_sample_rate(p: &Path) -> u32 {
    let b = std::fs::read(p).unwrap();
    assert_eq!(&b[0..4], b"RIFF");
    assert_eq!(&b[8..12], b"WAVE");
    assert_eq!(&b[12..16], b"fmt ");
    u32::from_le_bytes(b[24..28].try_into().unwrap())
}

fn sha256_of_file(p: &str) -> String {
    format!("{:x}", Sha256::digest(std::fs::read(p).unwrap()))
}

#[test]
fn normalises_mp3_to_48k_wav() {
    let s = normalise(Path::new("samples/track.mp3"), &tmpdir()).unwrap();
    assert_eq!(wav_sample_rate(&s.wav), 48_000);
    assert!((s.duration - 219.744).abs() < 0.05);
}

#[test]
fn reads_suno_id3_tags() {
    let s = normalise(Path::new("samples/track.mp3"), &tmpdir()).unwrap();
    assert_eq!(s.title.as_deref(), Some("Sunday Console Rite"));
    assert_eq!(s.artist.as_deref(), Some("tjdubz"));
}

#[test]
fn sha256_is_of_the_original_file_not_the_wav() {
    let a = normalise(Path::new("samples/track.mp3"), &tmpdir()).unwrap();
    let b = normalise(Path::new("samples/track.mp3"), &tmpdir()).unwrap();
    assert_eq!(a.sha256, b.sha256);
    assert_eq!(a.sha256, sha256_of_file("samples/track.mp3"));
    assert_ne!(a.sha256, sha256_of_file(a.wav.to_str().unwrap()));
}

#[test]
fn m4a_without_tags_has_none() {
    let s = normalise(Path::new("samples/three-note-signature.m4a"), &tmpdir()).unwrap();
    assert_eq!(s.title, None);
    assert_eq!(s.artist, None);
    assert_eq!(s.sha256, "14a2aeaf2cb59d4836e69bef8de4e45a9da2ee737be999f912ad9013ed4a752a");
}

#[test]
fn missing_ffmpeg_is_a_clear_error() {
    let e = normalise_with_ffmpeg(Path::new("samples/track.mp3"), &tmpdir(), "ffmpeg-does-not-exist");
    assert!(e.unwrap_err().to_string().contains("ffmpeg"));
}
