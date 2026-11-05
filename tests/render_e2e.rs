//! End-to-end: audio file in, mp4 out. Expectations about lengths come from
//! the *input* (ffprobe on the source), never from the output measuring itself.
//!
//! projectM renders are not reproducible run to run (see tests/headless_render.rs),
//! so nothing here compares pixels between two renders.

use projectmp4::render::plan::{render_plan, RenderArgs, RenderPlan};
use projectmp4::render::r#loop::{render_report, RenderReport};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

const MOTIF: &str = "samples/three-note-motif.m4a";
const SIGNATURE: &str = "samples/three-note-signature.m4a";
const FPS: u32 = 60;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn ffprobe_json(path: &Path, extra: &[&str]) -> serde_json::Value {
    let o = Command::new("ffprobe")
        .args(["-v", "error", "-of", "json"])
        .args(extra)
        .arg(path)
        .output()
        .unwrap();
    assert!(o.status.success(), "ffprobe: {}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

/// Duration of the *input* file, in seconds.
fn input_duration(path: &str) -> f64 {
    let j = ffprobe_json(Path::new(path), &["-show_entries", "format=duration"]);
    j["format"]["duration"].as_str().unwrap().parse().unwrap()
}

struct Probe {
    width: i64,
    height: i64,
    video_duration: f64,
    audio_duration: f64,
    video_frames: i64,
    r_frame_rate: String,
}

fn probe(out: &Path) -> Probe {
    let j = ffprobe_json(out, &["-count_frames", "-show_streams"]);
    let streams = j["streams"].as_array().unwrap();
    let find = |t: &str| streams.iter().find(|s| s["codec_type"] == t).unwrap_or_else(|| panic!("no {t} stream"));
    let (v, a) = (find("video"), find("audio"));
    Probe {
        width: v["width"].as_i64().unwrap(),
        height: v["height"].as_i64().unwrap(),
        video_duration: v["duration"].as_str().unwrap().parse().unwrap(),
        audio_duration: a["duration"].as_str().unwrap().parse().unwrap(),
        video_frames: v["nb_read_frames"].as_str().unwrap().parse().unwrap(),
        r_frame_rate: v["r_frame_rate"].as_str().unwrap().into(),
    }
}

/// Luma of the middle band of every output frame, 96 x 29. The band excludes
/// the title block (top) and the watermark (bottom), so only the visualiser is
/// measured. Decoded from the mp4, independent of the renderer's own checks.
const BAND_W: usize = 96;
const BAND_H: usize = 29;
fn band_frames(out: &Path) -> Vec<Vec<u8>> {
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(out)
        .args(["-vf", &format!("crop=1920:580:0:320,scale={BAND_W}:{BAND_H},format=gray")])
        .args(["-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .stderr(Stdio::inherit())
        .output()
        .unwrap();
    assert!(o.status.success());
    assert_eq!(o.stdout.len() % (BAND_W * BAND_H), 0);
    o.stdout.chunks(BAND_W * BAND_H).map(<[u8]>::to_vec).collect()
}

/// h264 black is luma 16 (limited range); allow codec noise above it.
const BLACK_LUMA: u8 = 24;

fn longest_black_run(frames: &[Vec<u8>]) -> usize {
    let (mut run, mut best) = (0, 0);
    for f in frames {
        if f.iter().all(|&p| p <= BLACK_LUMA) {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

/// Renders shared by more than one test. Each lives in a fixed directory under
/// `target/` that is wiped before it is rendered into, so at most one copy of
/// each ever exists on disk (a per-test temp dir cannot be dropped from a static).
fn shared_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn motif() -> &'static RenderReport {
    static R: OnceLock<RenderReport> = OnceLock::new();
    R.get_or_init(|| {
        let mut a = RenderArgs::new(MOTIF);
        a.out = Some(shared_dir("render_e2e_motif").join("motif.mp4"));
        render_report(&a).unwrap()
    })
}

/// Two presets projectM cannot use, then one it can. `fast` because the encoder,
/// not projectM, is where a render spends its time, and nothing here is about quality.
fn broken_then_good() -> &'static RenderReport {
    static R: OnceLock<RenderReport> = OnceLock::new();
    R.get_or_init(|| {
        let mut a = RenderArgs::new(SIGNATURE);
        a.presets = vec![fixture("unparsable.milk"), fixture("black.milk"), fixture("basic.milk")];
        a.out = Some(shared_dir("render_e2e_broken").join("out.mp4"));
        a.fast = true;
        render_report(&a).unwrap()
    })
}

#[test]
fn renders_a_short_track_end_to_end() {
    let r = motif();
    let p = probe(&r.out);
    let input = input_duration(MOTIF);
    assert_eq!((p.width, p.height), (1920, 1080));
    assert_eq!(p.r_frame_rate, "60/1");
    // The video covers the input to within the one frame that `ceil` adds.
    assert!(
        (p.video_duration - input).abs() <= 1.0 / FPS as f64,
        "video {} s vs input {input} s",
        p.video_duration
    );
    // The audio is the input, to within AAC's one-packet (1024 sample) padding.
    assert!((p.audio_duration - input).abs() <= 1024.0 / 48_000.0 + 1e-3, "audio {} s vs input {input} s", p.audio_duration);
    // Frame count against the input's length, not the output's own duration.
    let expect = (input * FPS as f64).ceil() as i64;
    assert!((p.video_frames - expect).abs() <= 1, "{} frames, expected {expect} +-1", p.video_frames);
}

#[test]
fn title_and_artist_default_to_the_tags() {
    let a = RenderArgs::new("samples/track.mp3");
    let plan = render_plan(&a).unwrap();
    assert_eq!(plan.title, "Sunday Console Rite");
    assert_eq!(plan.artist.as_deref(), Some("tjdubz"));

    // An explicit flag beats the tag; a file with no tags falls back to its name.
    let mut b = RenderArgs::new("samples/track.mp3");
    b.title = Some("Override".into());
    b.artist = Some("Someone".into());
    let plan = render_plan(&b).unwrap();
    assert_eq!((plan.title.as_str(), plan.artist.as_deref()), ("Override", Some("Someone")));

    let plan = render_plan(&RenderArgs::new(SIGNATURE)).unwrap();
    assert_eq!((plan.title.as_str(), plan.artist.as_deref()), ("three-note-signature", None));
}

#[test]
fn a_broken_preset_is_skipped_not_fatal() {
    let (unparsable, black, good) = (fixture("unparsable.milk"), fixture("black.milk"), fixture("basic.milk"));
    let r = broken_then_good();

    // Both broken presets were really first in line, so skipping them is not vacuous.
    assert_eq!(&r.plan.preset_sequence[..1], &[unparsable.clone()]);
    assert_eq!(r.plan.pool, [unparsable.clone(), black.clone(), good.clone()]);

    // Each was skipped, once, for its own reason: projectM refused the first,
    // and the second loaded but drew nothing for over a second.
    let skipped: Vec<(&PathBuf, &str)> = r.skipped.iter().map(|s| (&s.preset, s.reason.as_str())).collect();
    assert_eq!(skipped.len(), 2, "{skipped:?}");
    assert_eq!(skipped[0].0, &unparsable);
    assert!(skipped[0].1.contains("could not load"), "{}", skipped[0].1);
    assert_eq!(skipped[1].0, &black);
    assert!(skipped[1].1.contains("black"), "{}", skipped[1].1);
    // Only the good preset ran, for every segment.
    assert_eq!(r.played.len(), r.plan.preset_sequence.len());
    assert!(r.played.iter().all(|p| p == &good), "{:?}", r.played);
    // `-shortest` may trim the last frame when the AAC stream ends a few ms early.
    assert!((probe(&r.out).video_frames - r.frames as i64).abs() <= 1);

    // And the picture is the good preset's: not black, and not the idle logo
    // projectM shows when a load fails (which lights ~99% of pixels; basic.milk ~28%).
    let frames = band_frames(&r.out);
    let lit: Vec<f64> = frames
        .iter()
        .map(|f| f.iter().filter(|&&p| p > BLACK_LUMA).count() as f64 / f.len() as f64)
        .collect();
    let mean = lit.iter().sum::<f64>() / lit.len() as f64;
    assert!((0.02..0.8).contains(&mean), "mean lit fraction {mean:.3}");
    // The black preset was given up on exactly when its run of music-over-black
    // reached a second (`fps` frames), not earlier and not later...
    assert_eq!(r.max_black_run as usize, FPS as usize);
    // ...so the video is black for about that long, plus the replacement's own
    // dark fade-in (measured: a 62-frame run at luma <= 24), not for the whole track.
    let run = longest_black_run(&frames);
    assert!((FPS as usize - 5..FPS as usize + 15).contains(&run), "{run}");
}

/// Count of near-white pixels in a box of a full-resolution gray frame. The
/// overlay's text is pure white; basic.milk's own lines are dimmer.
fn white(frame: &[u8], x: std::ops::Range<usize>, y: std::ops::Range<usize>) -> usize {
    y.map(|y| frame[y * 1920 + x.start..y * 1920 + x.end].iter().filter(|&&p| p >= 235).count()).sum()
}

#[test]
fn output_is_upright_not_flipped() {
    // glReadPixels returns rows bottom-up, so the flip has two jobs, and each has
    // its own check. (1) The visualiser: top_line.milk puts one white square near
    // the top of the frame (MilkDrop y = 0.85, y up); without the flip it lands at
    // the bottom. (2) The overlay: it is composited after the flip, so the
    // watermark sits bottom-left and the title top-right. Compositing first and
    // flipping after moves them to top-left and bottom-right while leaving (1)
    // correct, so (1) alone would not catch it.
    let mut a = RenderArgs::new(SIGNATURE);
    a.presets = vec![fixture("top_line.milk")];
    a.out = Some(shared_dir("render_e2e_upright").join("out.mp4"));
    a.fast = true;
    let r = render_report(&a).unwrap();
    assert!(r.skipped.is_empty(), "{:?}", r.skipped);

    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(&r.out)
        .args(["-vf", "select='not(mod(n,60))'", "-vsync", "0", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output()
        .unwrap();
    assert!(o.status.success());
    let frames: Vec<&[u8]> = o.stdout.chunks(1920 * 1080).collect();
    assert!(frames.len() >= 10, "{} sampled frames", frames.len());
    // Skip frame 0: the square has not developed yet. Anything later must be steady.
    for (i, f) in frames.iter().enumerate().skip(1) {
        let (up, down) = (white(f, 560..1360, 40..420), white(f, 560..1360, 660..1040));
        assert!(up > 3000 && down < 100, "frame {i}: square {up} px in the top half vs {down} in the bottom half");
        let mark = white(f, 64..400, 930..1016);
        let mark_flipped = white(f, 64..400, 64..150);
        let title = white(f, 1400..1856, 70..130);
        let title_flipped = white(f, 1400..1856, 950..1010);
        assert!(mark > 1000 && mark_flipped < 100, "frame {i}: watermark {mark} bottom-left vs {mark_flipped} top-left");
        assert!(title > 1000 && title_flipped < 100, "frame {i}: title {title} top-right vs {title_flipped} bottom-right");
    }
}

fn plan_of(p: &str) -> RenderPlan {
    render_plan(&RenderArgs::new(p)).unwrap()
}

#[test]
fn plan_is_structurally_deterministic() {
    // NOT sha256 equality: libprojectM seeds hue offsets from std::random_device and noise
    // textures from the system clock, so two renders of one preset differ in ~28% of pixels.
    let a = plan_of(SIGNATURE);
    let b = plan_of(SIGNATURE);
    assert_eq!(a.switches, b.switches); // same times AND same Cut kinds
    assert_eq!(a.preset_sequence, b.preset_sequence);
    assert_eq!(a.total_frames, b.total_frames);

    // Equality of two empty or constant plans would pass the lines above.
    assert!(!a.switches.is_empty(), "a 12 s track at an 8 s gap has a switch");
    assert_eq!(a.preset_sequence.len(), a.switches.len() + 1);
    assert_ne!(a.preset_sequence[0], a.preset_sequence[1], "every segment got the same preset");
    // Sized by the decoded audio (12.0135 s: AAC padding), within a frame of the container's 12.000 s.
    let expect = (input_duration(SIGNATURE) * FPS as f64).ceil() as i64;
    assert!((a.total_frames as i64 - expect).abs() <= 1, "{} frames vs {expect}", a.total_frames);
}

#[test]
fn dump_plan_round_trips() {
    let plan = plan_of(SIGNATURE);
    let j = plan.to_json();
    let back = RenderPlan::from_json(&j).unwrap();
    assert_eq!(back.preset_sequence, plan.preset_sequence);
    assert_eq!(back, plan);

    // from_json reads the JSON rather than recomputing from the input.
    let mut v: serde_json::Value = serde_json::from_str(&j).unwrap();
    v["title"] = "edited".into();
    v["preset_sequence"].as_array_mut().unwrap().reverse();
    let edited = RenderPlan::from_json(&v.to_string()).unwrap();
    assert_eq!(edited.title, "edited");
    let mut rev = plan.preset_sequence.clone();
    rev.reverse();
    assert_eq!(edited.preset_sequence, rev);

    assert!(RenderPlan::from_json("{}").is_err());
}

#[test]
fn no_black_run_reaches_one_second() {
    // Many MilkDrop presets fade through black on a hard cut, so isolated black
    // frames are legitimate; a run of a second or more is a dead preset.
    let r = motif();
    assert!((r.max_black_run as usize) < FPS as usize, "renderer saw a {}-frame black run", r.max_black_run);
    let frames = band_frames(&r.out);
    assert!(frames.len() >= 1000, "decoded only {} frames", frames.len());
    let run = longest_black_run(&frames);
    assert!(run < FPS as usize, "decoded video has a {run}-frame black run");
}

/// Temp dir removed on drop (including during a panic unwind).
struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!("projectmp4-e2e-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&d).unwrap();
        Tmp(d)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 48 kHz stereo 16-bit WAV from `(seconds, loud)` segments: loud is two
/// decaying 60 Hz kicks per second, quiet is digital silence.
fn write_synth_wav(path: &Path, segments: &[(f64, bool)]) {
    let rate = 48_000usize;
    let mut pcm: Vec<u8> = Vec::new();
    for &(secs, loud) in segments {
        for i in 0..(secs * rate as f64) as usize {
            let t = i as f64 / rate as f64;
            let v = if loud {
                let ph = (t * 2.0) % 1.0;
                0.8 * (-ph * 12.0).exp() * (std::f64::consts::TAU * 60.0 * t).sin()
            } else {
                0.0
            };
            let q = ((v * 32767.0) as i16).to_le_bytes();
            pcm.extend_from_slice(&[q[0], q[1], q[0], q[1]]);
        }
    }
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    for v in [16u32, 0x0002_0001, rate as u32, rate as u32 * 4, 0x0010_0004] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    b.extend_from_slice(&pcm);
    std::fs::write(path, b).unwrap();
}

/// `n` distinct copies of a fixture inside `dir`, so a pool can hold many presets.
fn copies_of(name: &str, dir: &Path, n: usize) -> Vec<PathBuf> {
    (0..n)
        .map(|i| {
            let p = dir.join(format!("{name}-{i:02}.milk"));
            std::fs::copy(fixture(&format!("{name}.milk")), &p).unwrap();
            p
        })
        .collect()
}

#[test]
fn a_silent_gap_does_not_burn_through_the_preset_pool() {
    // Quiet music is normal (ambient), and every preset is dark in silence. Black
    // over silence is correct output and must not count against a preset. The
    // pool is 40 presets that always draw nothing, so each one is condemned after
    // a second of *audible* black: the 6 s of sound costs 6 of them. The 50 s of
    // silence that follows is longer than the ~41 s (40 presets x 61 frames) that
    // used to exhaust the pool and abort the render, and must cost none.
    let tmp = Tmp::new();
    let wav = tmp.0.join("gap.wav");
    write_synth_wav(&wav, &[(6.0, true), (50.0, false)]);
    let mut a = RenderArgs::new(&wav);
    a.presets = copies_of("black", &tmp.0, 40);
    a.out = Some(tmp.0.join("gap.mp4"));
    a.fast = true;
    let r = render_report(&a).expect("a silent stretch must not abort the render");
    assert!(r.out.exists());
    assert!(!r.pool_exhausted);
    assert!((5..=6).contains(&r.skipped.len()), "{} presets skipped, expected one per audible second", r.skipped.len());
    let expect = (input_duration(wav.to_str().unwrap()) * FPS as f64).ceil() as i64;
    assert!((probe(&r.out).video_frames - expect).abs() <= 1);
}

#[test]
fn running_out_of_presets_does_not_abort_the_render() {
    // Every candidate draws nothing while music plays: all are skipped, and the
    // render finishes anyway on the last one rather than throwing away its work.
    let tmp = Tmp::new();
    let wav = tmp.0.join("loud.wav");
    write_synth_wav(&wav, &[(6.0, true)]);
    let mut a = RenderArgs::new(&wav);
    a.presets = copies_of("black", &tmp.0, 2);
    a.out = Some(tmp.0.join("loud.mp4"));
    a.fast = true;
    let r = render_report(&a).expect("exhausting the pool must not abort");
    assert!(r.pool_exhausted);
    // One was skipped; the last candidate stays on screen instead of being skipped too.
    assert_eq!(r.skipped.len(), 1, "{:?}", r.skipped);
    assert_eq!(r.played.len(), 1);
    assert_eq!(probe(&r.out).video_frames, r.frames as i64);
}

/// Dominant colour of the middle of each requested output frame, as "red",
/// "blue" or "other".
fn colours_at(out: &Path, frames: &[u64]) -> Vec<&'static str> {
    let sel: Vec<String> = frames.iter().map(|n| format!("eq(n,{n})")).collect();
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(out)
        .args(["-vf", &format!("select='{}',crop=200:400:100:340,scale=4:2", sel.join("+")), "-vsync", "0"])
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .output()
        .unwrap();
    assert!(o.status.success());
    let per = 4 * 2 * 3;
    assert_eq!(o.stdout.len(), frames.len() * per, "decoded {} frames", o.stdout.len() / per);
    o.stdout
        .chunks(per)
        .map(|f| {
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for px in f.chunks(3) {
                r += px[0] as u32;
                g += px[1] as u32;
                b += px[2] as u32;
            }
            let n = (per / 3) as u32;
            let (r, g, b) = (r / n, g / n, b / n);
            if r > 200 && g < 60 && b < 60 {
                "red"
            } else if b > 200 && r < 60 && g < 60 {
                "blue"
            } else {
                "other"
            }
        })
        .collect()
}

#[test]
fn preset_switches_land_on_their_planned_frames() {
    // red and blue are solid, so the preset on screen is readable from the video.
    use projectmp4::audio::timeline::Cut;
    let (red, blue) = (fixture("red.milk"), fixture("blue.milk"));
    let mut a = RenderArgs::new("samples/ravezilla.m4a");
    a.presets = vec![red.clone(), blue.clone()];
    a.limit_seconds = Some(40.0);
    a.out = Some(shared_dir("render_e2e_switches").join("out.mp4"));
    a.fast = true;
    let r = render_report(&a).unwrap();

    // The first 40 s of ravezilla hold three Soft switches then a Hard one, so
    // both kinds are exercised. If the analysis ever changes, this says so.
    let planned: Vec<_> = r.plan.switches.iter().filter(|s| s.t < 40.0).collect();
    let kinds: Vec<Cut> = planned.iter().map(|s| s.cut).collect();
    assert_eq!(kinds, [Cut::Soft, Cut::Soft, Cut::Soft, Cut::Hard]);
    let want_frames: Vec<u64> = planned.iter().map(|s| (s.t * FPS as f64).ceil() as u64).collect();

    // What the loop did: each switch loaded on the first frame at or after its
    // time, alternating the two presets.
    let frames: Vec<u64> = r.loads.iter().map(|l| l.frame).collect();
    assert_eq!(frames, [vec![0], want_frames.clone()].concat(), "{:?}", r.loads);
    let presets: Vec<&PathBuf> = r.loads.iter().map(|l| &l.preset).collect();
    assert_eq!(presets, [&red, &blue, &red, &blue, &red]);
    let cuts: Vec<Option<Cut>> = r.loads.iter().map(|l| l.cut).collect();
    assert_eq!(cuts, [None, Some(Cut::Soft), Some(Cut::Soft), Some(Cut::Soft), Some(Cut::Hard)]);

    // What the viewer sees. A Hard cut is instant: the frame before still shows
    // the old preset, the frame itself the new one. Each Soft cut is the old
    // preset up to the frame before it (the blend then runs for seconds).
    let (s1, s2, s3, hard) = (want_frames[0], want_frames[1], want_frames[2], want_frames[3]);
    let seen = colours_at(&r.out, &[s1 - 1, s2 - 1, s3 - 1, hard - 1, hard]);
    assert_eq!(seen, ["red", "blue", "red", "blue", "red"], "frames {:?}", [s1 - 1, s2 - 1, s3 - 1, hard - 1, hard]);
}

#[test]
fn stems_backed_render_length_follows_the_audio_it_is_muxed_with() {
    // ravezilla has stems in the agent-deck cache. They are 8,277,120 frames; the
    // WAV that ffmpeg muxes (and `-shortest` trims to) is 8,277,768. The video
    // must be sized by the audio the encoder actually gets.
    let plan = render_plan(&RenderArgs::new("samples/ravezilla.m4a")).unwrap();
    assert_eq!(plan.driver, projectmp4::audio::DriverKind::Stems, "ravezilla's stems are missing from the cache");
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i", "samples/ravezilla.m4a", "-vn", "-ac", "2", "-ar", "48000", "-f", "f32le", "-"])
        .output()
        .unwrap();
    assert!(o.status.success());
    let audio_frames = o.stdout.len() / 8;
    assert_eq!(plan.total_frames, (audio_frames as f64 / 48_000.0 * FPS as f64).ceil() as u64);
}

#[test]
fn dump_plan_cli_writes_a_plan_with_the_chosen_settings() {
    let tmp = Tmp::new();
    let path = tmp.0.join("plan.json");
    let o = Command::new(env!("CARGO_BIN_EXE_projectmp4"))
        .args(["render", SIGNATURE, "--watermark-style", "sigil", "--static-overlay", "--fast", "--title", "T", "--dump-plan"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!tmp.0.join("three-note-signature.mp4").exists());
    let dumped = RenderPlan::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();

    let mut a = RenderArgs::new(SIGNATURE);
    a.watermark_style = projectmp4::render::MarkStyle::Sigil;
    a.static_overlay = true;
    a.fast = true;
    a.title = Some("T".into());
    assert_eq!(dumped, render_plan(&a).unwrap());
    assert!(dumped.static_overlay && dumped.fast && dumped.title == "T");
    assert!(dumped.packs_revision.is_some());
}
