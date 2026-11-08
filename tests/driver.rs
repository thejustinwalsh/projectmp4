#[path = "common/tmp.rs"]
mod tmp;
use tmp::Tmp;
use projectmp4::audio::{Driver, Source};
use std::path::{Path, PathBuf};

fn tmpdir() -> Tmp {
    Tmp::new("driver")
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

/// A source whose mix is `frames` stereo frames of `level`. The `Tmp` owns the wav.
fn mix_source(frames: usize, level: f32) -> (Tmp, Source) {
    let dir = tmpdir();
    let wav = dir.join("mix.wav");
    write_wav(&wav, &vec![level; frames * 2]);
    let src = Source { wav, sha256: SHA.into(), duration: 0.0, title: None, artist: None };
    (dir, src)
}

#[test]
fn loads_the_full_mix() {
    let d = Driver::load(&tmp::click_source(40.0, 120.0)).unwrap();
    assert_eq!(d.rate, 48_000);
    assert!(!d.samples().is_empty());
}

#[test]
fn the_driver_is_exactly_the_decoded_wav() {
    // The driver is a function of the audio alone: its length is the wav's, sample for sample.
    let (_dir, src) = mix_source(3, 0.1);
    let d = Driver::load(&src).unwrap();
    assert_eq!(d.samples(), &[0.1; 6]);
    assert_eq!(d.frames(), projectmp4::audio::wav_frames(&src.wav).unwrap() as usize);
}

#[test]
fn window_past_end_of_track_is_silent_not_a_panic() {
    let d = Driver::load(&tmp::click_source(40.0, 120.0)).unwrap();
    let past = d.window(1e6, 800);
    assert_eq!(past.len(), 1600);
    assert!(past.iter().all(|s| *s == 0.0));
    let inside = d.window(30.0, 800);
    assert_eq!(inside.len(), 1600);
    assert!(inside.iter().any(|s| *s != 0.0), "window inside the track is silent");
}

#[test]
fn window_is_exactly_min_frames_cap_for_every_input() {
    use projectmp4::audio::MAX_WINDOW_FRAMES;
    // 10 frames of 0.75 with default rate 48000: track is 10/48000 s long.
    let (_dir, src) = mix_source(10, 0.75);
    let d = Driver::load(&src).unwrap();
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
    // The "zero frames" row asserts over empty slices, so on its own a `window` that returned
    // an empty slice for every input would pass it. Same position, one frame: a live window.
    assert!(d.window(sec(2.0), 0).is_empty());
    assert_eq!(d.window(sec(2.0), 1), &[0.75, 0.75], "window is empty for every input");
}

// --- the WAV header that every duration is computed from -------------------------------

fn wav_with(dir: &Path, name: &str, tag: u16, channels: u16, rate: u32, bits: u16, data: &[u8]) -> PathBuf {
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&tag.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * channels as u32 * bits as u32 / 8).to_le_bytes());
    b.extend_from_slice(&(channels * bits / 8).to_le_bytes());
    b.extend_from_slice(&bits.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(data);
    let p = dir.join(name);
    std::fs::write(&p, b).unwrap();
    p
}

#[test]
fn wav_frames_counts_a_canonical_file_and_refuses_any_other_format() {
    use projectmp4::audio::wav_frames;
    let dir = tmpdir();
    let data = vec![0u8; 8 * 100];
    assert_eq!(wav_frames(&wav_with(&dir, "ok.wav", 3, 2, 48_000, 32, &data)).unwrap(), 100);
    assert_eq!(wav_frames(&wav_with(&dir, "empty.wav", 3, 2, 48_000, 32, &[])).unwrap(), 0);
    // Each of these would divide by the wrong frame size or rate if trusted.
    // The last field is what the message must say was *found*; the expectation text
    // ("expected 48000 Hz stereo 32-bit float") is in every message and proves nothing.
    for (what, tag, ch, rate, bits, found) in [
        ("44.1 kHz", 3, 2, 44_100, 32, "44100 Hz, 2 channel(s), 32-bit (format tag 3)"),
        ("mono", 3, 1, 48_000, 32, "48000 Hz, 1 channel(s), 32-bit (format tag 3)"),
        ("16-bit", 1, 2, 48_000, 16, "48000 Hz, 2 channel(s), 16-bit (format tag 1)"),
        ("32-bit integer", 1, 2, 48_000, 32, "48000 Hz, 2 channel(s), 32-bit (format tag 1)"),
        ("16-bit under a float tag", 3, 2, 48_000, 16, "48000 Hz, 2 channel(s), 16-bit (format tag 3)"),
    ] {
        let e = wav_frames(&wav_with(&dir, "bad.wav", tag, ch, rate, bits, &data)).expect_err(what);
        assert!(e.to_string().contains("not a normalised WAV") && e.to_string().contains(found), "{what}: {e}");
    }
}

#[test]
fn prepare_refuses_a_source_whose_wav_holds_no_samples() {
    // `normalise` never emits such a file, so the plan is handed one directly: this pins
    // that `prepare` itself calls the guard, not just that the guard works.
    use projectmp4::render::plan::{render_plan_from_source, RenderArgs};
    let dir = tmpdir();
    let wav = wav_with(&dir, "empty.wav", 3, 2, 48_000, 32, &[]);
    assert_eq!(projectmp4::audio::wav_frames(&wav).unwrap(), 0, "premise: a valid WAV, zero frames");
    let src = Source { wav, sha256: SHA.into(), duration: 0.0, title: None, artist: None };
    let e = render_plan_from_source(&RenderArgs::new("ambient.mp3"), src).expect_err("an empty decode produced a plan");
    assert!(e.to_string().contains("ambient.mp3") && e.to_string().contains("no audio samples"), "{e}");
}
