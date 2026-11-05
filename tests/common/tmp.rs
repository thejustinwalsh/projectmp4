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
