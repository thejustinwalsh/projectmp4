//! The control front end: reads real htdemucs stems from an agent-deck bundle. Lives under
//! `tests/common` so it is compiled only into tests and examples, never into the library or the
//! binary; projectmp4 itself must not be able to reach agent-deck's cache.
#![allow(dead_code)]

use crate::agentdeck;
use projectmp4::audio::sections::frontend::FourChannel;
use projectmp4::ffi::{Error, Result};
use std::path::{Path, PathBuf};

pub struct CachedStems {
    pub bundle: PathBuf,
}

impl CachedStems {
    pub fn new(bundle: &Path) -> Self {
        CachedStems { bundle: bundle.to_path_buf() }
    }
}

impl FourChannel for CachedStems {
    /// Ignores the samples of `mono`; uses only its length and the 48 kHz rate.
    fn channels(&self, mono: &[f32], sr: u32) -> Result<[Vec<f32>; 4]> {
        if sr != 48_000 {
            return Err(Error::msg("CachedStems holds 48 kHz stems"));
        }
        let read = |name: &str| {
            let mut v = agentdeck::read_wav48_mono(&self.bundle.join(format!("{name}.wav")));
            v.resize(mono.len(), 0.0);
            v
        };
        Ok([read("bass"), read("drums"), read("other"), read("vocals")])
    }
}
