use projectmp4::encode::{ffmpeg_args, Encoder};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Temp dir removed on drop, so failing tests do not leak.
struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "projectmp4-encode-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        Tmp(d)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const AUDIO: &str = "samples/track.mp3";

fn grey_frame(w: u32, h: u32) -> Vec<u8> {
    vec![128; w as usize * h as usize * 4]
}

struct Probe {
    width: i64,
    height: i64,
    pix_fmt: String,
    video_codec: String,
    audio_codec: String,
    video_frames: i64,
}

fn ffprobe(p: &Path) -> Probe {
    let o = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-of", "json", "-show_streams"])
        .arg(p)
        .output()
        .unwrap();
    assert!(o.status.success(), "ffprobe: {}", String::from_utf8_lossy(&o.stderr));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let streams = j["streams"].as_array().unwrap();
    let find = |t: &str| streams.iter().find(|s| s["codec_type"] == t).unwrap_or_else(|| panic!("no {t} stream"));
    let (v, a) = (find("video"), find("audio"));
    Probe {
        width: v["width"].as_i64().unwrap(),
        height: v["height"].as_i64().unwrap(),
        pix_fmt: v["pix_fmt"].as_str().unwrap().into(),
        video_codec: v["codec_name"].as_str().unwrap().into(),
        audio_codec: a["codec_name"].as_str().unwrap().into(),
        video_frames: v["nb_read_frames"].as_str().unwrap().parse().unwrap(),
    }
}

#[test]
fn produces_a_playable_1080p60_mp4_with_audio() {
    let t = Tmp::new();
    let out = t.0.join("o.mp4");
    let mut e = Encoder::start(&out, Path::new(AUDIO), 1920, 1080, 60, false).unwrap();
    for _ in 0..120 {
        e.write_frame(&grey_frame(1920, 1080)).unwrap();
    }
    e.finish().unwrap();
    let p = ffprobe(&out);
    assert_eq!((p.width, p.height), (1920, 1080));
    assert_eq!(p.pix_fmt, "yuv420p");
    assert_eq!(p.video_codec, "h264");
    assert_eq!(p.audio_codec, "aac");
    assert!((p.video_frames - 120).abs() <= 1, "frames {}", p.video_frames);
}

#[test]
fn fast_path_produces_h264_with_audio() {
    let t = Tmp::new();
    let out = t.0.join("f.mp4");
    let mut e = Encoder::start(&out, Path::new(AUDIO), 320, 180, 30, true).unwrap();
    for _ in 0..30 {
        e.write_frame(&grey_frame(320, 180)).unwrap();
    }
    e.finish().unwrap();
    let p = ffprobe(&out);
    assert_eq!((p.video_codec.as_str(), p.audio_codec.as_str()), ("h264", "aac"));
    assert!((p.video_frames - 30).abs() <= 1, "frames {}", p.video_frames);
}

#[test]
fn rate_control_flags_match_the_codec() {
    let args = |fast| -> Vec<String> {
        ffmpeg_args(Path::new("o.mp4"), Path::new("a.mp3"), 8, 8, 30, fast)
            .into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    };
    let has = |a: &[String], f: &str, v: &str| a.windows(2).any(|w| w[0] == f && w[1] == v);
    let (slow, fast) = (args(false), args(true));
    assert!(has(&slow, "-c:v", "libx264") && has(&slow, "-crf", "17") && has(&slow, "-preset", "slow"));
    assert!(has(&fast, "-c:v", "h264_videotoolbox") && has(&fast, "-q:v", "65"));
    // videotoolbox silently ignores these, so passing them would be inert.
    assert!(!fast.iter().any(|a| a == "-crf" || a == "-preset"), "{fast:?}");
}

#[test]
fn overwrites_an_existing_output_instead_of_reading_frames_as_the_answer() {
    let t = Tmp::new();
    let out = t.0.join("o.mp4");
    std::fs::write(&out, b"stale").unwrap();
    let mut e = Encoder::start(&out, Path::new(AUDIO), 320, 180, 30, false).unwrap();
    for _ in 0..10 {
        e.write_frame(&grey_frame(320, 180)).unwrap();
    }
    e.finish().unwrap();
    assert_eq!(ffprobe(&out).video_codec, "h264");
}

#[test]
fn rejects_a_wrong_sized_frame() {
    let t = Tmp::new();
    let mut e = Encoder::start(&t.0.join("o.mp4"), Path::new(AUDIO), 320, 180, 30, false).unwrap();
    let err = e.write_frame(&[0u8; 10]).unwrap_err();
    assert!(err.0.contains("10 bytes") && err.0.contains("230400"), "{}", err.0);
}

/// Writes frames until the first error, then `finish`; returns the first error text.
fn first_error(mut e: Encoder, frames: usize) -> String {
    for _ in 0..frames {
        if let Err(err) = e.write_frame(&grey_frame(320, 180)) {
            return err.0.into_owned();
        }
    }
    e.finish().expect_err("encoder failure must surface").0.into_owned()
}

#[test]
fn ffmpeg_rejecting_the_output_surfaces_its_stderr() {
    let t = Tmp::new();
    // Parent directory does not exist: ffmpeg starts, then fails to open the output.
    let out = t.0.join("missing-dir").join("o.mp4");
    let e = Encoder::start(&out, Path::new(AUDIO), 320, 180, 30, false).unwrap();
    let msg = first_error(e, 20);
    assert!(msg.contains(&out.display().to_string()), "path missing: {msg}");
    // Text that only ffmpeg's own stderr can supply.
    assert!(msg.contains("No such file or directory"), "stderr missing: {msg}");
}

#[test]
fn ffmpeg_dying_mid_stream_surfaces_after_real_frames_were_written() {
    let t = Tmp::new();
    let out = t.0.join("o.mp4");
    let mut e = Encoder::start(&out, Path::new(AUDIO), 320, 180, 30, false).unwrap();
    for i in 0..5 {
        e.write_frame(&grey_frame(320, 180)).unwrap_or_else(|err| panic!("frame {i}: {}", err.0));
    }
    let killed = Command::new("kill").args(["-9", &e.pid().to_string()]).status().unwrap();
    assert!(killed.success());
    let msg = first_error(e, 50);
    assert!(msg.contains("SIGKILL") || msg.contains("signal: 9"), "exit status missing: {msg}");
    assert!(msg.contains(&out.display().to_string()), "path missing: {msg}");
}
