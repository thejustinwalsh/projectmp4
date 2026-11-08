use projectmp4::audio::{normalise, normalise_with_ffmpeg};
use sha2::{Digest, Sha256};
#[path = "common/tmp.rs"]
mod tmp;
use tmp::Tmp;
use std::path::Path;

fn tmpdir() -> Tmp {
    Tmp::new("normalise")
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
    let path = tmp::click_file("mp3", 30.0, 120.0);
    let s = tmp::normalised(path.to_str().unwrap());
    assert_eq!(wav_sample_rate(&s.wav), 48_000);
    // 0.05 s is the tolerance this test has always carried, and it is the input
    // container's slack rather than the pipeline's: mp3 is not sample-exact, so a
    // 30 s source reports 30.024 s — one frame of encoder padding, 1152 samples at
    // 48 kHz. The decoded wav is tighter than that (30.000125 s, measured).
    assert!((s.duration - 30.0).abs() < 0.05, "duration {}", s.duration);
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

/// A stub ffmpeg that creates the `.part` output and then fails. Only the
/// cleanup in `normalise_with_ffmpeg` can make the `.part` assertion pass.
#[test]
fn ffmpeg_failure_after_writing_part_removes_it() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tmpdir();
    let input = dir.join("input.wav");
    std::fs::write(&input, b"placeholder input").unwrap();
    let sha = sha256_of_file(input.to_str().unwrap());
    let part = dir.join(format!("{sha}.wav.part"));
    let wav = dir.join(format!("{sha}.wav"));

    // The stub writes to its last argument (the output path), then drops a
    // marker so the test can tell the stub really ran and created the file.
    let marker = dir.join("stub-ran");
    let stub = dir.join("fake-ffmpeg.sh");
    let script = format!(
        "#!/bin/sh\n\
         for a in \"$@\"; do out=\"$a\"; done\n\
         printf partial > \"$out\" || exit 2\n\
         : > '{}'\n\
         exit 1\n",
        marker.display()
    );
    std::fs::write(&stub, script).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let e = normalise_with_ffmpeg(&input, &dir, stub.to_str().unwrap());
    assert!(e.is_err(), "stub exits 1, so normalise must fail");
    assert!(marker.exists(), "stub did not run to completion; .part check would be vacuous");
    assert!(!part.exists(), "failed normalise left {} behind", part.display());
    assert!(!wav.exists(), "failed normalise left {} behind", wav.display());
}

#[test]
fn reads_suno_id3_tags() {
    let Some(s) = tmp::sample("samples/track.mp3") else { return };
    assert_eq!(s.title.as_deref(), Some("Sunday Console Rite"));
    assert_eq!(s.artist.as_deref(), Some("tjdubz"));
}

#[test]
fn sha256_is_of_the_original_file_not_the_wav() {
    let (da, db) = (tmpdir(), tmpdir());
    let mp3 = tmp::click_file("mp3", 30.0, 120.0);
    let a = normalise(&mp3, &da).unwrap();
    let b = normalise(&mp3, &db).unwrap();
    assert_eq!(a.sha256, b.sha256);
    assert_eq!(a.sha256, sha256_of_file(mp3.to_str().unwrap()));
    assert_ne!(a.sha256, sha256_of_file(a.wav.to_str().unwrap()));
}

#[test]
fn m4a_without_tags_has_none() {
    let path = tmp::click_file("m4a", 12.0, 120.0);
    let s = tmp::normalised(path.to_str().unwrap());
    assert_eq!(s.title, None);
    assert_eq!(s.artist, None);
    assert_eq!(s.sha256, sha256_of_file(path.to_str().unwrap()));
}

#[test]
fn the_signature_m4a_has_no_tags_and_a_known_hash() {
    // About one particular file: the pinned hash only means something for it.
    let Some(s) = tmp::sample("samples/three-note-signature.m4a") else { return };
    assert_eq!(s.title, None);
    assert_eq!(s.artist, None);
    assert_eq!(s.sha256, "14a2aeaf2cb59d4836e69bef8de4e45a9da2ee737be999f912ad9013ed4a752a");
}

#[test]
fn missing_ffmpeg_is_a_clear_error() {
    let msg = normalise_with_ffmpeg(&tmp::click_file("mp3", 30.0, 120.0), &tmpdir(), "ffmpeg-does-not-exist")
        .unwrap_err()
        .to_string();
    // Not `contains("ffmpeg")`: the program name we passed already satisfies that.
    assert!(msg.contains("failed to run"), "{msg}");
    assert!(msg.contains("No such file or directory"), "the OS reason did not reach the user: {msg}");
}
