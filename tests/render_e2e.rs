//! End-to-end: audio file in, mp4 out. Expectations about lengths come from
//! the *input* (ffprobe on the source), never from the output measuring itself.
//!
//! projectM renders are not reproducible run to run (see tests/headless_render.rs),
//! so nothing here compares pixels between two renders.

use projectmp4::render::plan::{render_plan, RenderArgs, RenderPlan};
use projectmp4::render::r#loop::{render_report, RenderReport};
#[path = "common/tmp.rs"]
mod tmp;
use tmp::Tmp;
use std::path::{Path, PathBuf};
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

/// Frames the video should have: the audio as the encoder receives it (decoded to
/// 48 kHz stereo), measured here by a separate ffmpeg decode, times fps, rounded up.
fn expected_frames(input: &str) -> u64 {
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i", input, "-vn", "-ac", "2", "-ar", "48000", "-f", "f32le", "-"])
        .output()
        .unwrap();
    assert!(o.status.success());
    let secs = (o.stdout.len() / 8) as f64 / 48_000.0;
    (secs * FPS as f64 - 1e-9).ceil() as u64
}

/// Exact: the plan and the loop agree with the decoded audio to the frame. The
/// file may hold one frame fewer, never more: with `-shortest` ffmpeg ends the
/// video where the (AAC, slightly shorter) audio ends. Measured: signature
/// 721 written -> 720 kept, ravezilla 10348 -> 10347.
fn assert_frames(r: &RenderReport, input: &str) {
    let want = expected_frames(input);
    assert_eq!(r.plan.total_frames, want, "plan");
    assert_eq!(r.frames_written, want, "frames handed to the encoder");
    let kept = probe(&r.out).video_frames as u64;
    assert!(kept == want || kept + 1 == want, "file holds {kept} frames, {want} were written");
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
    let dir = tmp::scratch_root().join(name);
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
    // Frame count against the input's decoded audio, not the output's own duration.
    assert_frames(r, MOTIF);
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
    assert_frames(r, SIGNATURE);

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
    // The clip is 12.01 s with one soft cut at 8.46 s. projectM then crossfades for 3 s
    // with a random transition shader, which can blend the square away (samples at
    // 10 s and 11 s did, about half the time). Stop before the cut: the square is
    // only a meaningful assertion outside a transition.
    a.limit_seconds = Some(7.9);
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
    assert!(frames.len() >= 7, "{} sampled frames", frames.len());
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
    // Sized by the decoded audio (12.0135 s: AAC padding; the container says 12.000).
    assert_eq!(a.total_frames, expected_frames(SIGNATURE));
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

#[derive(Clone, Copy)]
enum Tone {
    /// Two decaying 60 Hz kicks per second, peak 0.8.
    Kicks,
    /// A steady 220 Hz sine at this peak amplitude (0.0056 is -45 dBFS).
    Pad(f64),
    Silence,
}

/// 48 kHz stereo 16-bit WAV from `(seconds, tone)` segments.
fn write_synth_wav(path: &Path, segments: &[(f64, Tone)]) {
    let rate = 48_000usize;
    let mut pcm: Vec<u8> = Vec::new();
    for &(secs, tone) in segments {
        for i in 0..(secs * rate as f64) as usize {
            let t = i as f64 / rate as f64;
            let v = match tone {
                Tone::Kicks => {
                    let ph = (t * 2.0) % 1.0;
                    0.8 * (-ph * 12.0).exp() * (std::f64::consts::TAU * 60.0 * t).sin()
                }
                Tone::Pad(amp) => amp * (std::f64::consts::TAU * 220.0 * t).sin(),
                Tone::Silence => 0.0,
            };
            let q = ((v * 32767.0).round() as i16).to_le_bytes();
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
    let tmp = Tmp::new("e2e");
    let wav = tmp.0.join("gap.wav");
    write_synth_wav(&wav, &[(6.0, Tone::Kicks), (50.0, Tone::Silence)]);
    let mut a = RenderArgs::new(&wav);
    a.presets = copies_of("black", &tmp.0, 40);
    a.out = Some(tmp.0.join("gap.mp4"));
    a.fast = true;
    let r = render_report(&a).expect("a silent stretch must not abort the render");
    assert!(r.out.exists());
    assert!(!r.pool_exhausted);
    assert!((5..=6).contains(&r.skipped.len()), "{} presets skipped, expected one per audible second", r.skipped.len());
    assert_frames(&r, wav.to_str().unwrap());
}

#[test]
fn running_out_of_presets_does_not_abort_the_render() {
    // Every candidate draws nothing while music plays: each is set aside, all are
    // re-admitted when none is left, and the render finishes as a complete file.
    let tmp = Tmp::new("e2e");
    let wav = tmp.0.join("loud.wav");
    write_synth_wav(&wav, &[(6.0, Tone::Kicks)]);
    let mut a = RenderArgs::new(&wav);
    let pool = copies_of("black", &tmp.0, 2);
    a.presets = pool.clone();
    a.out = Some(tmp.0.join("loud.mp4"));
    a.fast = true;
    let r = render_report(&a).expect("exhausting the pool must not abort");
    assert!(r.pool_exhausted);
    // Both were condemned, and the pool did not shrink to one: the second was
    // loaded again after the first was forgiven.
    for p in &pool {
        assert!(r.skipped.iter().any(|s| &s.preset == p), "{p:?} never skipped: {:?}", r.skipped);
    }
    assert!(r.loads.len() >= 4, "{:?}", r.loads);
    // At most one verdict per second of music (the run restarts after each): 6 s
    // gives at most 7 skips. Reloading every frame would give hundreds.
    assert!((2..=7).contains(&r.skipped.len()), "{} skips", r.skipped.len());
    assert_frames(&r, wav.to_str().unwrap());
    // A complete, playable file: the audio is there too (probe panics without it).
    assert!((probe(&r.out).audio_duration - 6.0).abs() < 0.05);
}

#[test]
fn ambient_music_does_not_leave_the_render_stuck_on_one_preset() {
    // A pad at -45 dBFS: far quieter than the kicks, well above the audibility
    // threshold. Presets that stay dark over it are condemned one a second; with 8
    // in the pool the pool is spent after ~8 s. A condemnation that never expired
    // would leave one preset on screen for the remaining 12 s; instead they are
    // forgiven and cycled, so presets keep changing for the whole track.
    let tmp = Tmp::new("e2e");
    let wav = tmp.0.join("pad.wav");
    write_synth_wav(&wav, &[(20.0, Tone::Pad(0.0056))]);
    let mut a = RenderArgs::new(&wav);
    a.presets = copies_of("black", &tmp.0, 8);
    a.out = Some(tmp.0.join("pad.mp4"));
    a.fast = true;
    let r = render_report(&a).expect("ambient music must not abort the render");
    // A steady pad has no beats: the plan falls back to a fixed 8 s grid (bpm 0).
    assert_eq!(r.plan.bpm, 0.0);
    assert_eq!(r.plan.switches.iter().map(|s| s.t).collect::<Vec<_>>(), [8.0, 16.0]);
    assert!(r.pool_exhausted);
    let distinct: std::collections::HashSet<_> = r.skipped.iter().map(|s| &s.preset).collect();
    assert_eq!(distinct.len(), 8);
    // Loads that were not planned switches: the opening one and each replacement
    // of a dark preset. Stuck after the pool ran out, there would be only 8.
    let replacements = r.loads.iter().filter(|l| l.cut.is_none()).count();
    assert!(replacements >= 15, "only {replacements} replacement loads: presets stopped changing");
    // One a second for 20 s, give or take the opening: not one per frame, not none.
    assert!((18..=21).contains(&r.skipped.len()), "{} skips", r.skipped.len());
    assert_frames(&r, wav.to_str().unwrap());
}

/// Mean RGB of the left-middle of each requested output frame (inside the
/// solid presets' border, clear of the overlay).
fn rgb_at(out: &Path, frames: &[u64]) -> Vec<[u32; 3]> {
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
            let mut m = [0u32; 3];
            for px in f.chunks(3) {
                for c in 0..3 {
                    m[c] += px[c] as u32;
                }
            }
            m.map(|v| v / 8)
        })
        .collect()
}

fn colours_at(out: &Path, frames: &[u64]) -> Vec<&'static str> {
    rgb_at(out, frames)
        .into_iter()
        .map(|[r, g, b]| {
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
    // This test reads the preset on screen frame by frame, including the instant
    // Hard cut. The flash guard exists to make that cut not instant (a red -> blue
    // jump is a full-frame flash), so it is off here; the guard has its own suites.
    a.flash_guard = false;
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
    // The flag actually handed to projectM: smooth for Soft, hard for Hard and the opening.
    let smooth: Vec<bool> = r.loads.iter().map(|l| l.smooth).collect();
    assert_eq!(smooth, [false, true, true, true, false]);
    let cuts: Vec<Option<Cut>> = r.loads.iter().map(|l| l.cut).collect();
    assert_eq!(cuts, [None, Some(Cut::Soft), Some(Cut::Soft), Some(Cut::Soft), Some(Cut::Hard)]);

    // What the viewer sees. A Hard cut is instant: the frame before still shows
    // the old preset, the frame itself the new one. Each Soft cut is the old
    // preset up to the frame before it (the blend then runs for seconds).
    let (s1, s2, s3, hard) = (want_frames[0], want_frames[1], want_frames[2], want_frames[3]);
    let seen = colours_at(&r.out, &[s1 - 1, s2 - 1, s3 - 1, hard - 1, hard]);
    assert_eq!(seen, ["red", "blue", "red", "blue", "red"], "frames {:?}", [s1 - 1, s2 - 1, s3 - 1, hard - 1, hard]);

    // A Soft cut blends for about three seconds, through a transition effect
    // projectM picks at random each time, so the blend's exact colours are not
    // reproducible; that there IS a blend, of these two colours, is. Every frame
    // of the 200 after each Soft switch is sampled. Blended frames are those
    // that are neither pure red nor pure blue; each must still be of the two
    // (no green), so a different preset does not count. The effect may fade
    // through near-black (measured: (0,0,15)), so brightness is not bounded.
    // Measured counts over 9 draws: 7 to 33 per switch; the bound is 4.
    // A switch handed to projectM without `smooth` would give none.
    for (&sf, new_colour) in want_frames[..3].iter().zip(["blue", "red", "blue"]) {
        let at: Vec<u64> = (0..200).map(|k| sf + k).collect();
        // The window ends on the pure new colour: the blend completed.
        assert_eq!(colours_at(&r.out, &[sf + 199]), [new_colour], "blend after frame {sf} did not finish");
        let blended: Vec<[u32; 3]> = rgb_at(&r.out, &at)
            .into_iter()
            .filter(|&[r, g, b]| !(r > 200 && g < 60 && b < 60) && !(b > 200 && r < 60 && g < 60))
            .collect();
        println!("blended frames after {sf}: {}", blended.len());
        // A fade passes through colours; a black flash is only black. Count the blended
        // frames that are not near-black (measured below).
        let lit = blended.iter().filter(|[r, g, b]| r + g + b >= 60).count();
        println!("after {sf}: {} blended, {lit} lit", blended.len());
        // Measured over 3 switches: every blended frame was lit (13 to 73). Black frames
        // pass the red/blue filter below, so without this a black flash would count as a blend.
        assert!(lit >= 4, "only {lit} of {} blended frames after frame {sf} are lit: a black flash, not a fade", blended.len());
        assert!(blended.len() >= 4, "only {} blended frames after the Soft switch at frame {sf}", blended.len());
        for [r, g, b] in &blended {
            assert!(*g < 80, "blended frame ({r},{g},{b}) is not red/blue, after frame {sf}");
        }
    }
    // A Hard cut is instant and complete: every sampled frame for 60 frames is
    // fully the new colour. A smooth flag on it would show a blend.
    let at: Vec<u64> = (0..20).map(|k| hard + 3 * k).collect();
    assert!(colours_at(&r.out, &at).iter().all(|&c| c == "red"), "{:?}", colours_at(&r.out, &at));
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

fn abs_sample(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("samples").join(name).to_str().unwrap().to_string()
}

fn pinned_cotc_commit() -> String {
    projectmp4::presets::packs::sources()
        .unwrap()
        .into_iter()
        .find(|s| s.name == "presets-cream-of-the-crop")
        .unwrap()
        .commit
}

#[test]
fn dump_plan_cli_writes_a_plan_and_renders_nothing() {
    let tmp = Tmp::new("e2e");
    let input = abs_sample("three-note-signature.m4a");
    let path = tmp.0.join("plan.json");
    // Run in the temp dir: a render would drop <stem>.mp4 into the working directory.
    let o = Command::new(env!("CARGO_BIN_EXE_projectmp4"))
        .current_dir(&tmp.0)
        .args(["render", &input, "--watermark-style", "sigil", "--static-overlay", "--fast", "--no-flash-guard", "--flash-limit", "0.02", "--title", "T", "--dump-plan"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut files: Vec<String> =
        std::fs::read_dir(&tmp.0).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    files.sort();
    assert_eq!(files, ["plan.json"], "a plan dump must not render");
    let dumped = RenderPlan::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();

    // The values asserted here are the literals typed on the command line above.
    assert_eq!(dumped.watermark_style, projectmp4::render::MarkStyle::Sigil);
    assert!(dumped.static_overlay);
    assert!(dumped.fast);
    assert!(!dumped.flash_guard);
    assert_eq!(dumped.flash_limit, 0.02);
    assert_eq!(dumped.title, "T");
    assert_eq!(dumped.packs_revision.as_deref(), Some(pinned_cotc_commit().as_str()));
    // And the same arguments through the library give the same plan.
    let mut a = RenderArgs::new(&input);
    a.watermark_style = projectmp4::render::MarkStyle::Sigil;
    a.static_overlay = true;
    a.fast = true;
    a.flash_guard = false;
    a.flash_limit = 0.02;
    a.title = Some("T".into());
    assert_eq!(dumped, render_plan(&a).unwrap());
    // The default is the wordmark: nothing selects the sigil unless asked.
    assert_eq!(render_plan(&RenderArgs::new(&input)).unwrap().watermark_style, projectmp4::render::MarkStyle::Word);
    // The guard is on, at the measured limit, unless switched off.
    let d = render_plan(&RenderArgs::new(&input)).unwrap();
    assert!(d.flash_guard && d.flash_limit == 0.010, "{} {}", d.flash_guard, d.flash_limit);
}

#[test]
fn a_plan_only_claims_a_pack_revision_when_it_used_the_packs() {
    let mut a = RenderArgs::new(SIGNATURE);
    assert_eq!(render_plan(&a).unwrap().packs_revision.as_deref(), Some(pinned_cotc_commit().as_str()));
    a.presets = vec![fixture("red.milk")];
    assert_eq!(render_plan(&a).unwrap().packs_revision, None);
}

#[test]
fn limit_seconds_rejects_nonsense_and_a_limited_plan_describes_itself() {
    let tmp = Tmp::new("e2e");
    let input = abs_sample("three-note-signature.m4a");
    for bad in ["0", "-1", "nan", "inf", "abc"] {
        let o = Command::new(env!("CARGO_BIN_EXE_projectmp4"))
            .current_dir(&tmp.0)
            .arg("render")
            .arg(&input)
            .arg(format!("--limit-seconds={bad}"))
            .output()
            .unwrap();
        assert!(!o.status.success(), "--limit-seconds={bad} was accepted");
        // Clap's own wrapper wording: only the argument parser emits "invalid value".
        // `render_plan` rejects the same inputs with a message that also names the flag, so
        // matching on "limit-seconds" alone would pass with the clap layer deleted.
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains("invalid value") && err.contains("limit-seconds"), "{bad}: {err}");
    }
    assert_eq!(std::fs::read_dir(&tmp.0).unwrap().count(), 0, "a rejected limit must not render");
    // The library refuses too (not only the CLI parser).
    for bad in [0.0, -3.0, f64::NAN, f64::INFINITY] {
        let mut a = RenderArgs::new(SIGNATURE);
        a.limit_seconds = Some(bad);
        assert!(render_plan(&a).is_err(), "{bad}");
    }

    // A 5 s limit on a 12 s track whose only switch is at 8.46 s: the plan says
    // 5 s, 300 frames and no switch, and records the limit.
    let mut a = RenderArgs::new(SIGNATURE);
    a.limit_seconds = Some(5.0);
    let p = render_plan(&a).unwrap();
    assert_eq!((p.duration, p.total_frames, p.limit_seconds), (5.0, 300, Some(5.0)));
    assert!(p.switches.is_empty());
    assert_eq!(p.preset_sequence.len(), 1);
    assert_eq!(RenderPlan::from_json(&p.to_json()).unwrap(), p);
    // A limit past the end changes nothing but the record of it.
    a.limit_seconds = Some(100.0);
    let full = render_plan(&a).unwrap();
    assert_eq!(full.total_frames, expected_frames(SIGNATURE));
    assert_eq!(full.switches.len(), 1);
}

/// A 6 s kick track under a fresh name, so the analysis cache cannot answer for it.
fn kicks_wav(tmp: &Tmp) -> PathBuf {
    let wav = tmp.0.join("kicks.wav");
    write_synth_wav(&wav, &[(6.0, Tone::Kicks)]);
    wav
}

#[test]
fn broken_analysis_models_are_an_error_not_a_fallback() {
    // The analyser could not run at all. Falling back to a grid would hide a
    // setup problem the user can fix, for every video, forever.
    let tmp = Tmp::new("e2e");
    let mut a = RenderArgs::new(kicks_wav(&tmp));
    a.models = tmp.0.join("no-such-models");
    let e = render_plan(&a).expect_err("a missing models directory must not fall back").to_string();
    // Phrases only this failure produces: not the fixture's own path, and not
    // what a beatless track says.
    assert!(e.contains("beat analysis could not run"), "{e}");
    assert!(e.contains("models are expected in"), "{e}");
    assert!(e.contains("no-such-models"), "the error should name the directory: {e}");
    assert!(!e.contains("no beats"), "{e}");
}

#[test]
fn a_beatless_track_plans_on_a_fixed_grid_when_the_analyser_works() {
    // The same working analyser, a track it finds no beats in: a fixed 8 s grid,
    // marked by bpm 0. The full render of this case is the ambient test.
    let tmp = Tmp::new("e2e");
    let wav = tmp.0.join("pad.wav");
    write_synth_wav(&wav, &[(20.0, Tone::Pad(0.0056))]);
    let p = render_plan(&RenderArgs::new(&wav)).expect("a beatless track must not abort");
    assert_eq!(p.bpm, 0.0);
    assert_eq!(p.switches.iter().map(|s| s.t).collect::<Vec<_>>(), [8.0, 16.0]);
    // ...and the analyser really is working on beaty material in the same setup.
    let k = render_plan(&RenderArgs::new(kicks_wav(&tmp))).unwrap();
    assert!(k.bpm > 0.0, "kick track bpm {}", k.bpm);
}

#[test]
fn a_plan_dumped_by_an_older_version_still_loads() {
    // Adding a field must never invalidate an existing plan: strip every field
    // added after the first version and the rest must load, with defaults.
    let plan = plan_of(SIGNATURE);
    let mut v: serde_json::Value = serde_json::from_str(&plan.to_json()).unwrap();
    for k in ["limit_seconds", "watermark_style", "static_overlay", "fast", "flash_guard", "flash_limit", "packs_revision"] {
        assert!(v.as_object_mut().unwrap().remove(k).is_some(), "{k} was not in the dump");
    }
    let old = RenderPlan::from_json(&v.to_string()).unwrap();
    assert_eq!(old.limit_seconds, None);
    assert_eq!(old.watermark_style, projectmp4::render::MarkStyle::Word);
    assert!(!old.static_overlay && !old.fast);
    // A plan that predates the guard replays with it on.
    assert!(old.flash_guard && old.flash_limit == 0.010);
    assert_eq!(old.packs_revision, None);
    assert_eq!(old.preset_sequence, plan.preset_sequence);
    assert_eq!(old.switches, plan.switches);
}

#[test]
fn a_valid_limit_seconds_renders_that_much_through_the_cli() {
    let tmp = Tmp::new("e2e");
    let out = tmp.0.join("short.mp4");
    let o = Command::new(env!("CARGO_BIN_EXE_projectmp4"))
        .current_dir(&tmp.0)
        .arg("render")
        .arg(abs_sample("three-note-signature.m4a"))
        .args(["--limit-seconds", "2", "--fast", "--preset"])
        .arg(fixture("red.milk"))
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let p = probe(&out);
    // 2 s at 60 fps is 120 frames; `-shortest` may trim one.
    assert!(p.video_frames == 120 || p.video_frames == 119, "{} frames", p.video_frames);
    assert!((p.video_duration - 2.0).abs() < 0.05, "{} s", p.video_duration);
}

#[test]
fn an_unusable_frame_rate_is_rejected_not_looped_on() {
    let tmp = Tmp::new("e2e");
    let input = abs_sample("three-note-signature.m4a");
    for bad in ["0", "1", "9", "241", "1000", "abc"] {
        let o = Command::new(env!("CARGO_BIN_EXE_projectmp4"))
            .current_dir(&tmp.0)
            .arg("render")
            .arg(&input)
            .arg(format!("--fps={bad}"))
            .output()
            .unwrap();
        assert!(!o.status.success(), "--fps={bad} was accepted");
        // Refused by the argument parser itself (clap's wording), before anything runs.
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains("--fps") && (err.contains("not in 10..=240") || err.contains("invalid digit")), "{bad}: {err}");
    }
    assert_eq!(std::fs::read_dir(&tmp.0).unwrap().count(), 0);
    for bad in [0u32, 1, 9, 241] {
        let mut a = RenderArgs::new(SIGNATURE);
        a.fps = bad;
        assert!(render_plan(&a).is_err(), "fps {bad}");
    }
    let mut a = RenderArgs::new(SIGNATURE);
    a.fps = 10;
    assert_eq!(render_plan(&a).unwrap().fps, 10);
}

#[test]
fn help_warns_that_preset_motion_depends_on_the_frame_rate() {
    let o = Command::new(env!("CARGO_BIN_EXE_projectmp4")).args(["render", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&o.stdout).replace('\n', " ");
    let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(help.contains("once per frame") && help.contains("slower at 10 fps") && help.contains("faster at 240"), "{help}");
}

#[test]
fn a_vbr_mp3_whose_container_duration_is_wrong_is_still_beat_analysed() {
    // A VBR MP3 without a Xing/Info header has only an estimated container
    // duration (here 18.4 s for 20.04 s of audio). The analyser runs on the decoded
    // audio, so judging it against the container's figure would distrust a good
    // analysis and fall back to the grid, blaming the analyser.
    let tmp = Tmp::new("e2e");
    let mp3 = tmp.0.join("vbr.mp3");
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", "sine=f=60:d=3,volume=0.8", "-f", "lavfi", "-i", "anoisesrc=d=20:a=0.3:seed=1"])
        .args(["-filter_complex", "[1:a]volume='if(lt(mod(t,0.5),0.1),1,0.02)':eval=frame[n];[n][0:a]amix=inputs=2:duration=longest"])
        .args(["-c:a", "libmp3lame", "-q:a", "9", "-write_xing", "0"])
        .arg(&mp3)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // The precondition that makes this test meaningful: the two durations disagree by over a second.
    let container = input_duration(mp3.to_str().unwrap());
    let decoded = expected_frames(mp3.to_str().unwrap()) as f64 / FPS as f64;
    assert!((container - decoded).abs() > 1.0, "container {container} s vs decoded {decoded} s no longer differ");
    let p = render_plan(&RenderArgs::new(&mp3)).unwrap();
    assert!(p.bpm > 0.0, "the beat analysis was thrown away (bpm {})", p.bpm);
    assert!(!p.switches.is_empty());
}
