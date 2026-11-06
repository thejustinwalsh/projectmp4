//! The signal that drives the visuals: the full mix, decoded from the
//! normalised WAV. It is a function of the audio alone, so a track renders the
//! same on every machine; nothing outside the repository is consulted.

use crate::audio::normalise::PIPELINE_RATE;
use crate::audio::Source;
use crate::ffi::{Error, Result};

#[derive(Debug, Clone)]
pub struct Driver {
    pub rate: u32,
    /// Interleaved stereo f32 at `rate`, followed by `MAX_WINDOW_FRAMES * 2`
    /// zeros. Private: the tail is padding, not signal. Use `samples()`.
    pcm: Vec<f32>,
    /// True length in stereo frames, excluding the padding.
    frames: usize,
}

/// Largest `frames` `window` honours; larger requests are clamped to it. Also
/// the length of the zero tail appended to the buffer at load.
pub const MAX_WINDOW_FRAMES: usize = 1 << 16;

impl Driver {
    pub(crate) fn new(mut pcm: Vec<f32>) -> Driver {
        let frames = pcm.len() / 2;
        pcm.truncate(frames * 2);
        pcm.resize(frames * 2 + MAX_WINDOW_FRAMES * 2, 0.0);
        Driver { rate: PIPELINE_RATE, pcm, frames }
    }

    /// True length in stereo frames (padding excluded).
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// The decoded signal, interleaved stereo, without the zero padding.
    pub fn samples(&self) -> &[f32] {
        &self.pcm[..self.frames * 2]
    }

    /// Decodes the normalised WAV into the driving signal.
    pub fn load(src: &Source) -> Result<Driver> {
        load_mix(src)
    }

    /// Exactly `min(frames, MAX_WINDOW_FRAMES) * 2` interleaved samples
    /// starting at `t` seconds, for every `t` and `frames`. `frames` is clamped
    /// to `MAX_WINDOW_FRAMES`. Any part of the window outside the track
    /// (negative, NaN, infinite or past-the-end `t`, or a straddling window)
    /// is zeros; this never panics.
    pub fn window(&self, t: f64, frames: usize) -> &[f32] {
        let n = frames.min(MAX_WINDOW_FRAMES) * 2;
        let end = self.frames * 2;
        // Compare as f64 before any cast that could saturate.
        let pos = t * self.rate as f64;
        let start = if pos >= 0.0 && pos < self.frames as f64 { pos as usize * 2 } else { end };
        // start <= end, so start + n <= end + MAX_WINDOW_FRAMES * 2 == pcm.len().
        &self.pcm[start..start + n]
    }
}

fn load_mix(src: &Source) -> Result<Driver> {
    let mut pcm = Vec::new();
    deck_media_codec::visit_stereo(&src.wav, |f| {
        pcm.extend_from_slice(f);
        Ok(())
    })
    .map_err(|e| Error::msg(format!("cannot decode {}: {e}", src.wav.display())))?;
    Ok(Driver::new(pcm))
}
