//! Scratch space for test suites. Shared by every suite that needs a temp dir.
//!
//! Two rules keep disk use bounded, by construction rather than by discipline:
//!   * `Tmp` removes its directory in `Drop`, so a panic or early return cleans up.
//!   * `normalised` makes one wav per source file per process, in a fixed
//!     directory under `target/tmp` (staged per process), instead of one copy per test.
#![allow(dead_code)]

use projectmp4::audio::{normalise, Source};
use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// Cargo's per-package scratch root for integration tests (`target/tmp`).
pub fn scratch_root() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
}

/// A unique directory under `target/tmp`, removed on drop (including during a panic unwind).
pub struct Tmp(pub PathBuf);

impl Tmp {
    pub fn new(tag: &str) -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = scratch_root().join(format!("{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&d).unwrap();
        Tmp(d)
    }
}

impl Deref for Tmp {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Tmp {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `input` normalised once per process into `target/tmp/normalised`. Tests that
/// only need a `Source` share one wav per input; tests of `normalise` itself
/// call it directly with a `Tmp`.
///
/// `cargo test` runs suite binaries concurrently, and `normalise` stages at a fixed
/// `{sha}.wav.part`, so two processes sharing one staging directory would truncate
/// each other's ffmpeg output. Each process therefore stages in its own
/// `stage-{pid}` directory and only the finished wav is renamed (atomically, same
/// filesystem) into the shared directory, where identical content is harmless.
pub fn normalised(input: &str) -> Source {
    static CACHE: OnceLock<Mutex<HashMap<String, Source>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    cache
        .entry(input.to_string())
        .or_insert_with(|| {
            let shared = scratch_root().join("normalised");
            let stage = shared.join(format!("stage-{}", std::process::id()));
            std::fs::create_dir_all(&stage)
                .unwrap_or_else(|e| panic!("cannot create staging dir {}: {e}", stage.display()));
            let mut src = normalise(Path::new(input), &stage)
                .unwrap_or_else(|e| panic!("normalising fixture {input} failed: {e}"));
            let published = shared.join(src.wav.file_name().unwrap());
            // A failure here is a harness problem (another process cannot be racing this
            // staging directory), not a disk fault in the code under test.
            std::fs::rename(&src.wav, &published).unwrap_or_else(|e| {
                panic!(
                    "test-harness error, not a product bug: could not publish normalised fixture \
                     {} -> {}: {e}",
                    src.wav.display(),
                    published.display()
                )
            });
            let _ = std::fs::remove_dir_all(&stage);
            src.wav = published;
            src
        })
        .clone()
}

/// A synthetic click track: `secs` seconds at exactly `bpm`, in `ext`'s
/// container, built once per process by ffmpeg.
///
/// Most suites need *an* audio file rather than a particular one: an encoder
/// needs something to mux, a driver needs something decodable. Those use this,
/// so they run in any checkout. It is a click track rather than a plain sine
/// because the pipeline's first step is beat analysis, and a tone with no
/// transients has no beat grid to find: a 55 Hz kick every 0.5 s with a 3 kHz
/// hat on the eighths gives the analyser both a tempo and a phase to lock to.
/// Only a test whose claim is about the *music* — a tagged title, a specific
/// duration, a file's hash, the section structure of a real arrangement —
/// needs `sample` below.
///
/// **Use 120 or 150 for `bpm` unless you measure another one first.** What the
/// analyser recovers is not always what was asked for, in two ways that are
/// properties of beat tracking rather than bugs:
///
///   * It quantises to its own hop grid (~0.02 s), so a beat period that is
///     not a multiple of that cannot come back exact. 140 measured 142.86
///     (0.42 s = 21 hops) and 92 measured 90.91 (0.66 s = 33 hops).
///   * It can pick the wrong metrical level. 100 and 75 both measured double,
///     having locked onto the hat train as the beat.
///
/// 120 (0.5 s) measured 120.000 and 150 (0.4 s) measured 150.0001 — both on
/// the grid and both unambiguous. Those two are the tested pair, and they are
/// enough for "a track" and "a different track".
///
/// The bar accent makes the downbeats a strict subset of the beats, so the
/// structural claims (a subset, non-empty, ascending, inside the duration)
/// hold — but **do not assert an exact bar count.** The analyser recovers the
/// bar grid exactly only when it has enough of it: 60 s at 120 BPM measured
/// 120 beats and 30 downbeats, exactly 30 bars, while 12 s at 120 BPM measured
/// 24 beats and 17 downbeats. A short fixture is enough to prove a subset and
/// not enough to prove a meter.
///
/// Staged per process and renamed into a shared directory for the reason given
/// on `normalised`: suite binaries run concurrently and must not truncate each
/// other's ffmpeg output.
pub fn click_file(ext: &str, secs: f64, bpm: f64) -> PathBuf {
    static CACHE: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    let name = format!("click-{}bpm-{}s.{ext}", bpm as u64, secs as u64);
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    cache
        .entry(name.clone())
        .or_insert_with(|| {
            let shared = scratch_root().join("clicks");
            let stage = shared.join(format!("stage-{}", std::process::id()));
            std::fs::create_dir_all(&stage)
                .unwrap_or_else(|e| panic!("cannot create staging dir {}: {e}", stage.display()));
            let staged = stage.join(&name);
            // One kick per beat, one hat per eighth, one deeper accent per bar of
            // four: tempo, phase, and a bar line. The accent is what makes the
            // downbeats a strict subset of the beats — without it every kick is
            // identical, the analyser has no bar to find, and it returns every
            // beat as a downbeat (measured: 60 of 60 at 30 s).
            let (beat, eighth, bar) = (60.0 / bpm, 30.0 / bpm, 240.0 / bpm);
            let expr = format!(
                "0.6*sin(2*PI*55*t)*exp(-14*mod(t,{beat})) \
                 + 0.25*sin(2*PI*3000*t)*exp(-60*mod(t,{eighth})) \
                 + 0.8*sin(2*PI*38*t)*exp(-6*mod(t,{bar}))"
            );
            let st = std::process::Command::new("ffmpeg")
                .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
                .arg(format!("aevalsrc='{expr}':d={secs}:s=48000:c=stereo"))
                .args(["-ar", "48000", "-ac", "2"])
                .arg(&staged)
                .arg("-y")
                .status()
                .unwrap_or_else(|e| panic!("cannot run ffmpeg to build fixture {name}: {e}"));
            assert!(st.success(), "ffmpeg could not build fixture {name}");
            let published = shared.join(&name);
            std::fs::rename(&staged, &published).unwrap_or_else(|e| {
                panic!("test-harness error, not a product bug: could not publish fixture {name}: {e}")
            });
            let _ = std::fs::remove_dir_all(&stage);
            published
        })
        .clone()
}

/// `click_file` normalised, for the suites that want a `Source`.
pub fn click_source(secs: f64, bpm: f64) -> Source {
    let f = click_file("wav", secs, bpm);
    normalised(f.to_str().expect("fixture path is utf-8"))
}

/// `normalised`, but `None` when the fixture is not in this checkout.
///
/// `samples/` is gitignored: it holds the maintainer's own music, which is not
/// ours to redistribute, so a clone of this public repository has none of it
/// and there is no step a contributor could take to obtain it. A test whose
/// claim is about that music therefore cannot run, and that is not a failure —
/// it has nothing to run against. Such a test skips, and says so.
///
/// Everything that only needs *some* audio uses `click_source` instead and
/// runs everywhere, so this is the narrow case, not the common one.
///
/// `PROJECTMP4_REQUIRE_SAMPLES=1` turns the skip back into a failure, for a
/// machine where the files are meant to be present: an unread skip is
/// indistinguishable from a pass, and on the maintainer's own box a missing
/// sample is a broken checkout, not an absent optional extra.
pub fn sample(input: &str) -> Option<Source> {
    if Path::new(input).exists() {
        return Some(normalised(input));
    }
    assert!(
        std::env::var_os("PROJECTMP4_REQUIRE_SAMPLES").is_none(),
        "{input} is missing and PROJECTMP4_REQUIRE_SAMPLES is set: this checkout is \
         supposed to have the maintainer's samples in it."
    );
    eprintln!(
        "SKIPPED: {input} is not in this checkout. samples/ is gitignored (the maintainer's \
         own music); tests whose claim is about that audio cannot run here. Everything that \
         needs only some audio uses a generated fixture and ran."
    );
    None
}
