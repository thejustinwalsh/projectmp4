//! Input normalisation: any audio file -> 48 kHz stereo f32 WAV, plus the
//! sha256 of the *original* file and its title/artist tags.
//!
//! This runs before analysis because `deck-media-codec` cannot decode mp3.
//! The ffmpeg call here is the only resample in the pipeline.

use crate::ffi::{Error, Result};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const PIPELINE_RATE: u32 = 48_000;

#[derive(Debug, Clone)]
pub struct Source {
    pub wav: PathBuf,
    /// sha256 (lowercase hex) of the original input file.
    pub sha256: String,
    pub duration: f64,
    pub title: Option<String>,
    pub artist: Option<String>,
}

pub fn normalise(input: &Path, tmp: &Path) -> Result<Source> {
    normalise_with_ffmpeg(input, tmp, "ffmpeg")
}

pub fn normalise_with_ffmpeg(input: &Path, tmp: &Path, ffmpeg: &str) -> Result<Source> {
    let sha256 = sha256_streaming(input)?;
    let wav = tmp.join(format!("{sha256}.wav"));
    let part = tmp.join(format!("{sha256}.wav.part"));

    // ffmpeg writes to `.part`; only a zero exit publishes it at the final path.
    let result = run_ffmpeg(input, &part, ffmpeg)
        .and_then(|()| probe(input))
        .and_then(|probed| {
            std::fs::rename(&part, &wav).map_err(|e| {
                Error::msg(format!("cannot move {} to {}: {e}", part.display(), wav.display()))
            })?;
            Ok(probed)
        });
    match result {
        Ok((duration, title, artist)) => Ok(Source { wav, sha256, duration, title, artist }),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

fn run_ffmpeg(input: &Path, out_path: &Path, ffmpeg: &str) -> Result<()> {
    let out = Command::new(ffmpeg)
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(input)
        .args(["-vn", "-ac", "2", "-ar"])
        .arg(PIPELINE_RATE.to_string())
        // Explicit muxer: the `.part` extension gives ffmpeg nothing to infer from.
        .args(["-c:a", "pcm_f32le", "-f", "wav"])
        .arg(out_path)
        .output()
        .map_err(|e| Error::msg(format!("failed to run {ffmpeg}: {e}")))?;
    if !out.status.success() {
        return Err(Error::msg(format!(
            "ffmpeg failed ({}) on {}: {}",
            out.status,
            input.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

fn sha256_streaming(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| Error::msg(format!("cannot open {}: {e}", path.display())))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| Error::msg(format!("cannot read {}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

fn probe(input: &Path) -> Result<(f64, Option<String>, Option<String>)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration"])
        .args(["-show_entries", "format_tags=title,artist", "-of", "json"])
        .arg("-i")
        .arg(input)
        .output()
        .map_err(|e| Error::msg(format!("failed to run ffprobe: {e}")))?;
    if !out.status.success() {
        return Err(Error::msg(format!(
            "ffprobe failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| Error::msg(format!("ffprobe emitted invalid JSON: {e}")))?;
    let fmt = &v["format"];
    let duration = fmt["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or_else(|| Error::msg("ffprobe reported no duration"))?;
    // Tag key case varies by container (TITLE vs title).
    let tag = |name: &str| {
        fmt["tags"].as_object().and_then(|t| {
            t.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .and_then(|(_, v)| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
    };
    Ok((duration, tag("title"), tag("artist")))
}
