//! Positive proof that nothing a sibling application left on disk can change what a render
//! sees. Each test plants the exact shape that used to be honoured (a valid analysis-cache
//! entry; a complete stem bundle) under a temporary `$HOME`, then asserts the result is the
//! computed one, not the planted one.
//!
//! `HOME` is process-global, so the two tests take one lock and put it back afterwards.
#[path = "common/tmp.rs"]
mod tmp;
use projectmp4::audio::analysis::DEFAULT_MODELS;
use projectmp4::audio::{analyse_outcome, wav_frames, Driver, Outcome, Source};
use std::path::Path;
use std::sync::Mutex;
use tmp::Tmp;

static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Runs `f` with `$HOME` pointed at a fresh directory, restoring it after (even on panic).
fn with_home<T>(f: impl FnOnce(&Path) -> T) -> T {
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = Tmp::new("no-agent-deck-home");
    let _restore = Restore(std::env::var_os("HOME"));
    std::env::set_var("HOME", &*home);
    f(&home)
}

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

#[test]
fn a_planted_agent_deck_cache_that_would_have_hit_is_ignored() {
    // 150 BPM so the planted 120 below is distinguishable from the real answer.
    let src = tmp::click_source(30.0, 150.0);
    let secs = wav_frames(&src.wav).unwrap() as f64 / 48_000.0;
    let planted = [1.0, 2.0, 3.0, 4.0];

    let got = with_home(|home| {
        let dir = home.join("Library/Application Support/agent-deck/analysis");
        std::fs::create_dir_all(&dir).unwrap();
        // Valid in every way the old reader checked: ascending downbeats inside the
        // duration, the duration equal to the track's, beats present.
        let entry = serde_json::json!({
            "bpm": 120.0,
            "beats": [0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0],
            "downbeats": planted,
            "duration": secs,
        });
        std::fs::write(dir.join(format!("{}.json", src.sha256)), entry.to_string()).unwrap();
        analyse_outcome(&src, Path::new(DEFAULT_MODELS), Some(secs)).unwrap()
    });

    let Outcome::Beats(a) = got else { panic!("the fixture has beats: {got:?}") };
    assert_ne!(a.downbeats, planted, "the planted agent-deck entry was used instead of the analyser");
    assert!(a.downbeats.len() > planted.len(), "a real analysis of a full track has many downbeats");
    assert_ne!(a.bpm, 120.0, "the planted bpm leaked through");
}

#[test]
fn a_planted_agent_deck_stem_bundle_is_ignored() {
    let dir = Tmp::new("no-agent-deck-src");
    let wav = dir.join("mix.wav");
    let mix = [0.1f32; 6];
    write_wav(&wav, &mix);
    let sha = "abc123";
    let src = Source { wav, sha256: sha.into(), duration: 0.0, title: None, artist: None };

    let d = with_home(|home| {
        let root = home.join("Library/Application Support/agent-deck/stems");
        let bundle = root.join("bundles/job1");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::create_dir_all(root.join("requests")).unwrap();
        // Same length as the track, so the old length guard would have trusted them.
        write_wav(&bundle.join("drums.wav"), &[0.5; 6]);
        write_wav(&bundle.join("bass.wav"), &[0.25; 6]);
        let stem = |role: &str| {
            serde_json::json!({"role": role, "sampleRate": 48000, "channels": 2, "frames": 3,
                "path": bundle.join(format!("{role}.wav")).to_str().unwrap()})
        };
        let manifest = serde_json::json!({"schema": 1, "completion": "complete",
            "sourceSha256": sha, "stems": [stem("drums"), stem("bass")]});
        std::fs::write(bundle.join("manifest.json"), manifest.to_string()).unwrap();
        std::fs::write(root.join("requests").join(format!("{sha}.json")), r#"{"jobId":"job1"}"#).unwrap();
        Driver::load(&src).unwrap()
    });

    // The summed stems would be [0.75; 6]. The mix is the only thing left to load.
    assert_eq!(d.samples(), &mix, "the planted stem bundle drove the visuals");
}
