use projectmp4::encode::{ffmpeg_args, Encoder};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc;
use std::time::Duration;

#[path = "common/tmp.rs"]
mod tmp;
use tmp::Tmp;

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
    r_frame_rate: String,
    encoder: String,
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
        r_frame_rate: v["r_frame_rate"].as_str().unwrap().into(),
        encoder: v["tags"]["encoder"].as_str().unwrap_or("").into(),
        video_frames: v["nb_read_frames"].as_str().unwrap().parse().unwrap(),
    }
}

#[test]
fn produces_a_playable_1080p60_mp4_with_audio() {
    let t = Tmp::new("encode");
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
    assert!(p.encoder.contains("libx264"), "encoder tag: {}", p.encoder);
    assert_eq!(p.r_frame_rate, "60/1");
    assert_eq!(p.audio_codec, "aac");
    assert!((p.video_frames - 120).abs() <= 1, "frames {}", p.video_frames);
}

/// Top-level mp4 box types with their byte offsets, in file order.
fn top_level_boxes(bytes: &[u8]) -> Vec<(String, usize)> {
    let (mut at, mut out) = (0usize, Vec::new());
    while at + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        out.push((String::from_utf8_lossy(&bytes[at + 4..at + 8]).into_owned(), at));
        // size 1 means a 64-bit size follows the type; 0 means "to end of file".
        let len = match size {
            0 => bytes.len() - at,
            1 => u64::from_be_bytes(bytes[at + 8..at + 16].try_into().unwrap()) as usize,
            n => n,
        };
        assert!(len >= 8, "corrupt box at {at}");
        at += len;
    }
    out
}

/// `-movflags +faststart` moves the index (`moov`) ahead of the media (`mdat`), which is what
/// lets Discord, X and browsers start playing before the file has fully downloaded.
#[test]
fn the_output_is_faststart_with_moov_before_mdat() {
    for fast in [false, true] {
        let t = Tmp::new("encode");
        let out = t.0.join("o.mp4");
        let mut e = Encoder::start(&out, Path::new(AUDIO), 320, 180, 30, fast).unwrap();
        for _ in 0..30 {
            e.write_frame(&grey_frame(320, 180)).unwrap();
        }
        e.finish().unwrap();
        let boxes = top_level_boxes(&std::fs::read(&out).unwrap());
        let at = |name: &str| boxes.iter().find(|(n, _)| n == name).map(|b| b.1);
        let (moov, mdat) = (at("moov"), at("mdat"));
        assert!(moov.is_some() && mdat.is_some(), "fast={fast}: boxes {boxes:?}");
        assert!(moov < mdat, "fast={fast}: moov must precede mdat for progressive playback: {boxes:?}");
        if fast {
            continue;
        }
        // Control: the same streams remuxed without the flag put moov last, so this parser
        // really does tell the two layouts apart (a file that always passed would prove nothing).
        let plain = t.0.join("plain.mp4");
        let o = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-y", "-i"])
            .arg(&out)
            .args(["-c", "copy"])
            .arg(&plain)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let boxes = top_level_boxes(&std::fs::read(&plain).unwrap());
        let at = |name: &str| boxes.iter().find(|(n, _)| n == name).map(|b| b.1);
        assert!(at("moov") > at("mdat"), "control: a non-faststart remux should end with moov: {boxes:?}");
    }
}

#[test]
fn fast_path_produces_h264_with_audio() {
    let t = Tmp::new("encode");
    let out = t.0.join("f.mp4");
    let mut e = Encoder::start(&out, Path::new(AUDIO), 320, 180, 30, true).unwrap();
    for _ in 0..30 {
        e.write_frame(&grey_frame(320, 180)).unwrap();
    }
    e.finish().unwrap();
    let p = ffprobe(&out);
    assert_eq!((p.video_codec.as_str(), p.audio_codec.as_str()), ("h264", "aac"));
    assert!(p.encoder.contains("h264_videotoolbox"), "encoder tag: {}", p.encoder);
    assert_eq!(p.r_frame_rate, "30/1");
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
    let t = Tmp::new("encode");
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
    let t = Tmp::new("encode");
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
    let t = Tmp::new("encode");
    // Parent directory does not exist: ffmpeg starts, then fails to open the output.
    let out = t.0.join("missing-dir").join("o.mp4");
    let e = Encoder::start(&out, Path::new(AUDIO), 320, 180, 30, false).unwrap();
    let msg = first_error(e, 20);
    assert!(msg.contains(&out.display().to_string()), "path missing: {msg}");
    // Text that only ffmpeg's own stderr can supply.
    assert!(msg.contains("No such file or directory"), "stderr missing: {msg}");
}

/// Write an executable `sh` stub standing in for ffmpeg. A watchdog kills it
/// after 20 s so a failed run cannot leave it behind; fds are redirected so the
/// watchdog does not hold the parent's pipes open. On normal exit the stub
/// kills its own watchdog, so the pid is never signalled after it is released.
/// `$OUT` is the last argument (the `.part` path).
fn stub(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    let script = format!(
        "#!/bin/sh\n(sleep 20 >/dev/null 2>&1 </dev/null && kill -9 $$) >/dev/null 2>&1 </dev/null &\nWD=$!\ntrap 'kill $WD 2>/dev/null' EXIT\nfor OUT; do :; done\n{body}\n"
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

/// ~260 KB on stderr (4x the pipe buffer) *before* stdin is read: a parent that
/// does not drain stderr deadlocks. FIRST/LAST markers pin the tail bound.
#[test]
fn stderr_is_drained_and_only_its_tail_is_kept() {
    let t = Tmp::new("encode");
    let program = stub(
        &t.0,
        "s",
        "echo FIRST-MARKER >&2\nyes xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx | head -c 260000 >&2\necho >&2\necho LAST-MARKER >&2\ncat >/dev/null\nexit 1",
    );
    let out = t.0.join("o.mp4");
    let (tx, rx) = mpsc::channel();
    let out2 = out.clone();
    std::thread::spawn(move || {
        let r = (|| {
            let mut e = Encoder::start_with_program(&program, &out2, Path::new(AUDIO), 320, 180, 30, false)?;
            for _ in 0..2 {
                e.write_frame(&grey_frame(320, 180))?;
            }
            e.finish()
        })();
        let _ = tx.send(r.map_err(|e| e.0.into_owned()));
    });
    let res = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("encoder hung: stderr is not being drained");
    let msg = res.expect_err("stub exits 1");
    assert!(msg.contains("LAST-MARKER"), "tail lost: {}", &msg[msg.len().saturating_sub(300)..]);
    assert!(!msg.contains("FIRST-MARKER"), "tail not bounded: {} bytes", msg.len());
    assert!(msg.len() < 8 * 1024 + 512, "{} bytes", msg.len());
    assert!(msg.contains("exit status: 1"), "{msg}");
}

/// A child that accepts exactly two frames, then fails on its own.
#[test]
fn ffmpeg_failing_on_its_own_after_accepting_frames_surfaces() {
    let t = Tmp::new("encode");
    let program = stub(&t.0, "s", "head -c 460800 >/dev/null\necho 'stub: boom' >&2\nexit 3");
    let out = t.0.join("o.mp4");
    let mut e = Encoder::start_with_program(&program, &out, Path::new(AUDIO), 320, 180, 30, false).unwrap();
    for i in 0..2 {
        e.write_frame(&grey_frame(320, 180)).unwrap_or_else(|err| panic!("frame {i}: {}", err.0));
    }
    let msg = first_error(e, 50);
    assert!(msg.contains("exit status: 3"), "status missing: {msg}");
    assert!(msg.contains("stub: boom"), "stderr missing: {msg}");
    assert!(msg.contains(&out.display().to_string()), "path missing: {msg}");
}

/// Staging files left next to `out` (`<name>.part.*`).
fn parts(out: &Path) -> Vec<PathBuf> {
    let prefix = format!("{}.part", out.file_name().unwrap().to_string_lossy());
    std::fs::read_dir(out.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(&prefix))
        .collect()
}

#[test]
fn exit_zero_without_a_fresh_output_file_is_an_error() {
    let t = Tmp::new("encode");
    // Exit 0 having written nothing / an empty file.
    let nothing = stub(&t.0, "nothing", "cat >/dev/null\nexit 0");
    let empty = stub(&t.0, "empty", "cat >/dev/null\n: > \"$OUT\"\nexit 0");

    let out = t.0.join("none.mp4");
    let mut e = Encoder::start_with_program(&nothing, &out, Path::new(AUDIO), 16, 16, 30, false).unwrap();
    e.write_frame(&grey_frame(16, 16)).unwrap();
    let msg = e.finish().unwrap_err().0.into_owned();
    assert!(msg.contains(&out.display().to_string()) && msg.contains("missing"), "{msg}");
    assert!(!out.exists(), "failed encode published a file");
    assert!(parts(&out).is_empty(), ".part left behind");

    let out = t.0.join("zero-length.mp4");
    let e = Encoder::start_with_program(&empty, &out, Path::new(AUDIO), 16, 16, 30, false).unwrap();
    let msg = e.finish().unwrap_err().0.into_owned();
    assert!(msg.contains("wrote an empty"), "{msg}");
    assert!(!out.exists(), "failed encode published a file");
    assert!(parts(&out).is_empty(), ".part left behind");

    // A previous good video survives a failed re-render untouched.
    let out = t.0.join("prev.mp4");
    std::fs::write(&out, b"old video").unwrap();
    let e = Encoder::start_with_program(&nothing, &out, Path::new(AUDIO), 16, 16, 30, false).unwrap();
    assert!(e.finish().is_err());
    assert_eq!(std::fs::read(&out).unwrap(), b"old video");
    assert!(parts(&out).is_empty(), ".part left behind");
}

#[test]
fn success_publishes_the_part_at_the_final_path() {
    let t = Tmp::new("encode");
    let ok = stub(&t.0, "ok", "cat >/dev/null\necho video > \"$OUT\"\nexit 0");
    let out = t.0.join("o.mp4");
    let e = Encoder::start_with_program(&ok, &out, Path::new(AUDIO), 16, 16, 30, false).unwrap();
    assert!(!out.exists());
    e.finish().unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"video\n");
    assert!(parts(&out).is_empty());
}

#[test]
fn failure_after_frames_leaves_neither_part_nor_output() {
    let t = Tmp::new("encode");
    let program = stub(&t.0, "s", "echo partial > \"$OUT\"\nhead -c 460800 >/dev/null\nexit 3");
    let out = t.0.join("o.mp4");
    let mut e = Encoder::start_with_program(&program, &out, Path::new(AUDIO), 320, 180, 30, false).unwrap();
    for _ in 0..2 {
        e.write_frame(&grey_frame(320, 180)).unwrap();
    }
    let _ = first_error(e, 50);
    assert!(!out.exists() && parts(&out).is_empty());
}

#[test]
fn dropping_an_unfinished_encoder_removes_the_part() {
    let t = Tmp::new("encode");
    let program = stub(&t.0, "s", "echo partial > \"$OUT\"\ncat >/dev/null");
    let out = t.0.join("o.mp4");
    let e = Encoder::start_with_program(&program, &out, Path::new(AUDIO), 16, 16, 30, false).unwrap();
    // Wait for the stub to create the .part before dropping.
    for _ in 0..200 {
        if !parts(&out).is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!parts(&out).is_empty(), "stub never created the .part");
    drop(e);
    assert!(parts(&out).is_empty() && !out.exists());
}

#[test]
fn absurd_dimensions_are_rejected_not_wrapped() {
    let t = Tmp::new("encode");
    let r = Encoder::start(&t.0.join("o.mp4"), Path::new(AUDIO), u32::MAX, u32::MAX, 30, false);
    assert!(r.is_err());
    for (w, h) in [(0, 1080), (1920, 0), (0, 0)] {
        let e = Encoder::start(&t.0.join("z.mp4"), Path::new(AUDIO), w, h, 30, false).err().expect("zero size must be rejected");
        assert!(e.0.contains(&format!("{w}x{h}")), "{}", e.0);
    }
}

#[test]
fn a_stale_staging_file_is_never_published() {
    let t = Tmp::new("encode");
    let nothing = stub(&t.0, "nothing", "cat >/dev/null\nexit 0");
    let out = t.0.join("o.mp4");
    let part = t.0.join("o.mp4.part.stale");
    std::fs::write(&part, b"stale bytes").unwrap();
    let e = Encoder::start_staged(&nothing, &out, &part, Path::new(AUDIO), 16, 16, 30, false).unwrap();
    assert!(e.finish().is_err(), "stale staging file passed for output");
    assert!(!out.exists(), "stale bytes published");
    assert!(!part.exists());
}

#[test]
fn an_unremovable_staging_path_is_an_error_not_ignored() {
    let t = Tmp::new("encode");
    let nothing = stub(&t.0, "nothing", "cat >/dev/null\nexit 0");
    let out = t.0.join("o.mp4");
    // A non-empty directory cannot be removed with remove_file.
    let part = t.0.join("o.mp4.part.dir");
    std::fs::create_dir(&part).unwrap();
    std::fs::write(part.join("x"), b"x").unwrap();
    let r = Encoder::start_staged(&nothing, &out, &part, Path::new(AUDIO), 16, 16, 30, false);
    let msg = r.err().expect("removal failure must be reported").0.into_owned();
    assert!(msg.contains("o.mp4.part.dir"), "{msg}");
    assert!(!out.exists());
}

/// Two live encoders to one output each stage privately: finishing one can
/// only publish its own bytes.
#[test]
fn concurrent_renders_to_one_output_do_not_share_staging() {
    let t = Tmp::new("encode");
    let dir = t.0.display();
    let mk = |tag: &str| {
        stub(
            &t.0,
            tag,
            &format!("echo {tag} > \"$OUT\"\ntouch '{dir}/ready-{tag}'\ncat >/dev/null\nexit 0"),
        )
    };
    let (pa, pb) = (mk("A"), mk("B"));
    let out = t.0.join("o.mp4");
    let ea = Encoder::start_with_program(&pa, &out, Path::new(AUDIO), 16, 16, 30, false).unwrap();
    let eb = Encoder::start_with_program(&pb, &out, Path::new(AUDIO), 16, 16, 30, false).unwrap();
    for _ in 0..500 {
        if t.0.join("ready-A").exists() && t.0.join("ready-B").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(t.0.join("ready-A").exists() && t.0.join("ready-B").exists(), "stubs never wrote");
    assert_eq!(parts(&out).len(), 2, "live encoders must hold distinct staging files: {:?}", parts(&out));
    ea.finish().unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"A\n", "A published someone else's bytes");
    eb.finish().unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"B\n");
    assert!(parts(&out).is_empty());
}

#[test]
fn a_staging_path_that_is_the_output_is_rejected_and_the_old_video_survives() {
    let t = Tmp::new("encode");
    let nothing = stub(&t.0, "nothing", "cat >/dev/null\nexit 0");
    let out = t.0.join("o.mp4");
    std::fs::write(&out, b"previous render").unwrap();
    let r = Encoder::start_staged(&nothing, &out, &out, Path::new(AUDIO), 16, 16, 30, false);
    // Destructive consequence first: the old video must be byte-identical.
    assert_eq!(std::fs::read(&out).ok().as_deref(), Some(&b"previous render"[..]), "existing output was destroyed");
    let msg = r.err().expect("part == out must be rejected").0.into_owned();
    assert!(msg.contains("staging path") && msg.contains(&out.display().to_string()), "{msg}");
}

#[test]
fn a_staging_path_in_another_directory_is_rejected() {
    let t = Tmp::new("encode");
    let nothing = stub(&t.0, "nothing", "cat >/dev/null\nexit 0");
    let sub = t.0.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let out = t.0.join("o.mp4");
    let part = sub.join("o.mp4.part.x");
    let msg = Encoder::start_staged(&nothing, &out, &part, Path::new(AUDIO), 16, 16, 30, false)
        .err()
        .expect("cross-directory staging must be rejected")
        .0
        .into_owned();
    assert!(msg.contains(&part.display().to_string()) && msg.contains(&out.display().to_string()), "{msg}");
}
