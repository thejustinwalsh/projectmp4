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

/// (format tag, channels, sample rate) from the WAV `fmt ` chunk.
fn wav_fmt(p: &Path) -> (u16, u16, u32) {
    let b = std::fs::read(p).unwrap();
    assert_eq!(&b[0..4], b"RIFF");
    assert_eq!(&b[8..12], b"WAVE");
    let mut i = 12;
    while i + 8 <= b.len() {
        let size = u32::from_le_bytes(b[i + 4..i + 8].try_into().unwrap()) as usize;
        if &b[i..i + 4] == b"fmt " {
            let d = i + 8;
            return (
                u16::from_le_bytes(b[d..d + 2].try_into().unwrap()),
                u16::from_le_bytes(b[d + 2..d + 4].try_into().unwrap()),
                u32::from_le_bytes(b[d + 4..d + 8].try_into().unwrap()),
            );
        }
        i += 8 + size + (size & 1);
    }
    panic!("no fmt chunk in {}", p.display());
}

fn wav_sample_rate(p: &Path) -> u32 {
    wav_fmt(p).2
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
fn resamples_44k_mono_to_48k_stereo_float() {
    let dir = tmpdir();
    let src = dir.join("src44k.wav");
    let st = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
        .args(["-ar", "44100", "-ac", "1"])
        .arg(&src)
        .arg("-y")
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(wav_fmt(&src).2, 44_100, "premise: source must not be 48 kHz");

    let s = normalise(&src, &dir).unwrap();
    let (tag, channels, rate) = wav_fmt(&s.wav);
    assert_eq!(rate, 48_000, "output sample rate");
    assert_eq!(channels, 2, "output channels");
    assert_eq!(tag, 3, "output format tag (3 = IEEE float)");
}

#[test]
fn failed_normalise_leaves_no_output() {
    let dir = tmpdir();
    let bad = dir.join("garbage.wav");
    std::fs::write(&bad, b"not audio at all").unwrap();
    let sha = sha256_of_file(bad.to_str().unwrap());
    let e = normalise(&bad, &dir);
    assert!(e.is_err());
    assert!(!dir.join(format!("{sha}.wav")).exists());
    assert!(!dir.join(format!("{sha}.wav.part")).exists());
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
