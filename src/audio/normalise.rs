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

/// Where a normalised WAV's samples are: the `data` chunk's byte offset and its
/// frame count.
///
/// The `fmt ` chunk is read and must say 48 kHz, stereo, 32-bit float: the
/// frame size (8 bytes) and the rate both enter every time computed from this
/// count, so a file that is anything else is an error here rather than a
/// quietly wrong duration there. Chunks other than `fmt ` and `data` (ffmpeg
/// writes a `LIST`) are skipped, so the offset is not 44 in general.
fn wav_data(path: &Path) -> Result<(u64, u64)> {
    use std::io::{Seek, SeekFrom};
    let err = |e: std::io::Error| Error::msg(format!("read {}: {e}", path.display()));
    let bad = |what: String| Error::msg(format!("{} is not a normalised WAV: {what}", path.display()));
    let mut f = std::fs::File::open(path).map_err(err)?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).map_err(err)?;
    if &head[..4] != b"RIFF" || &head[8..] != b"WAVE" {
        return Err(Error::msg(format!("{} is not a WAV file", path.display())));
    }
    let mut fmt_seen = false;
    loop {
        let mut ch = [0u8; 8];
        f.read_exact(&mut ch).map_err(err)?;
        let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
        if &ch[..4] == b"fmt " {
            if size < 16 {
                return Err(bad(format!("fmt chunk of {size} bytes")));
            }
            let mut fmt = [0u8; 16];
            f.read_exact(&mut fmt).map_err(err)?;
            let tag = u16::from_le_bytes([fmt[0], fmt[1]]);
            let channels = u16::from_le_bytes([fmt[2], fmt[3]]);
            let rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
            let bits = u16::from_le_bytes([fmt[14], fmt[15]]);
            // 3 = IEEE float; 0xFFFE = extensible, which carries the real tag further on.
            if !(tag == 3 || tag == 0xFFFE) || channels != 2 || bits != 32 || rate != PIPELINE_RATE {
                return Err(bad(format!(
                    "{rate} Hz, {channels} channel(s), {bits}-bit (format tag {tag}); expected {PIPELINE_RATE} Hz stereo 32-bit float"
                )));
            }
            fmt_seen = true;
            f.seek(SeekFrom::Current((((size - 16) + 1) & !1) as i64)).map_err(err)?;
            continue;
        }
        if &ch[..4] == b"data" {
            if !fmt_seen {
                return Err(bad("data chunk before the fmt chunk".into()));
            }
            // ffmpeg streaming writes may leave 0xFFFFFFFF; fall back to the file length.
            let len = f.metadata().map_err(err)?.len();
            let pos = f.stream_position().map_err(err)?;
            let bytes = if size == 0xFFFF_FFFF || pos + size > len { len - pos } else { size };
            return Ok((pos, bytes / 8));
        }
        f.seek(SeekFrom::Current(((size + 1) & !1) as i64)).map_err(err)?;
    }
}

/// Stereo frames in a normalised WAV, from its `data` chunk size. See [`wav_data`]
/// for what the file must be.
pub fn wav_frames(path: &Path) -> Result<u64> {
    wav_data(path).map(|(_, frames)| frames)
}

/// Frames copied per read while cutting a clip: bounds memory at 32 KiB however
/// long the source is.
const TRIM_BLOCK_FRAMES: usize = 4096;

/// Cut `src` to `start..end` seconds and write the result as a new pipeline
/// WAV in `tmp`. `fade` seconds of raised-cosine fade are applied at each edge
/// that is a real cut: the fade-in only when `start` is past the first sample,
/// the fade-out only when `end` is before the last. Tags and the original
/// file's sha256 carry over; `duration` becomes the clip's length.
pub fn trim(src: &Source, start: f64, end: f64, fade: f64, tmp: &Path) -> Result<Source> {
    if !start.is_finite() || !end.is_finite() || !fade.is_finite() {
        return Err(Error::msg(format!("cannot trim to {start}..{end} s with a {fade} s fade: all three must be finite")));
    }
    if fade < 0.0 {
        return Err(Error::msg(format!("cannot trim with a {fade} s fade: a fade cannot be negative")));
    }
    if start < 0.0 || end <= start {
        return Err(Error::msg(format!("cannot trim to {start}..{end} s: need 0 <= start < end")));
    }
    let (data_at, total) = wav_data(&src.wav)?;
    let s0 = ((start * PIPELINE_RATE as f64).round() as u64).min(total);
    let s1 = ((end * PIPELINE_RATE as f64).round() as u64).min(total);
    if s1 <= s0 {
        return Err(Error::msg(format!(
            "cannot trim to {start}..{end} s: {} has {total} frames ({:.3} s), so the span is empty",
            src.wav.display(),
            total as f64 / PIPELINE_RATE as f64
        )));
    }
    let frames = s1 - s0;
    // (frames - 1) / 2, not frames / 2: at two frames a one-frame fade on each side
    // would take both to zero.
    let fade_frames = (((fade * PIPELINE_RATE as f64).round() as u64).min((frames - 1) / 2)) as usize;
    let fade_in = if s0 > 0 { fade_frames } else { 0 };
    let fade_out = if s1 < total { fade_frames } else { 0 };

    let wav = tmp.join(format!("{}-clip-{s0}-{s1}.wav", src.sha256));
    let part = tmp.join(format!("{}-clip-{s0}-{s1}.wav.part", src.sha256));
    let result = copy_span(&src.wav, data_at + s0 * 8, frames, fade_in, fade_out, &part)
        .and_then(|()| {
            std::fs::rename(&part, &wav).map_err(|e| {
                Error::msg(format!("cannot move {} to {}: {e}", part.display(), wav.display()))
            })
        });
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    Ok(Source {
        wav,
        sha256: src.sha256.clone(),
        duration: frames as f64 / PIPELINE_RATE as f64,
        title: src.title.clone(),
        artist: src.artist.clone(),
    })
}

/// Write `frames` frames from byte `from` of `src` to `out` as a canonical WAV,
/// shaping the first `fade_in` and last `fade_out` frames.
#[allow(clippy::chunks_exact_to_as_chunks)] // blocks are a whole number of 8-byte frames by construction
fn copy_span(src: &Path, from: u64, frames: u64, fade_in: usize, fade_out: usize, out: &Path) -> Result<()> {
    use std::io::{BufWriter, Seek, SeekFrom, Write};
    let rerr = |e: std::io::Error| Error::msg(format!("read {}: {e}", src.display()));
    let werr = |e: std::io::Error| Error::msg(format!("write {}: {e}", out.display()));
    let data_len = u32::try_from(frames * 8)
        .map_err(|_| Error::msg(format!("a {frames}-frame clip does not fit a WAV data chunk")))?;
    let riff_len = u32::try_from(frames * 8 + 36)
        .map_err(|_| Error::msg(format!("a {frames}-frame clip does not fit a WAV file (4 GiB limit)")))?;
    let mut input = std::fs::File::open(src).map_err(rerr)?;
    input.seek(SeekFrom::Start(from)).map_err(rerr)?;
    let mut w = BufWriter::new(std::fs::File::create(out).map_err(werr)?);

    let mut head = Vec::with_capacity(44);
    head.extend_from_slice(b"RIFF");
    head.extend_from_slice(&riff_len.to_le_bytes());
    head.extend_from_slice(b"WAVEfmt ");
    head.extend_from_slice(&16u32.to_le_bytes());
    head.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    head.extend_from_slice(&2u16.to_le_bytes());
    head.extend_from_slice(&PIPELINE_RATE.to_le_bytes());
    head.extend_from_slice(&(PIPELINE_RATE * 8).to_le_bytes());
    head.extend_from_slice(&8u16.to_le_bytes());
    head.extend_from_slice(&32u16.to_le_bytes());
    head.extend_from_slice(b"data");
    head.extend_from_slice(&data_len.to_le_bytes());
    w.write_all(&head).map_err(werr)?;

    // Same raised cosine as the stem overlap-add window in `stems::span_weight`.
    let ramp = |i: usize, f: usize| 0.5 - 0.5 * (std::f64::consts::PI * i as f64 / f as f64).cos();
    let mut buf = vec![0u8; TRIM_BLOCK_FRAMES * 8];
    let mut done = 0u64;
    while done < frames {
        let n = (frames - done).min(TRIM_BLOCK_FRAMES as u64) as usize;
        let block = &mut buf[..n * 8];
        input.read_exact(block).map_err(rerr)?;
        for (j, frame) in block.chunks_exact_mut(8).enumerate() {
            let at = done + j as u64;
            let mut gain = 1.0f64;
            if (at as usize) < fade_in {
                gain *= ramp(at as usize, fade_in);
            }
            let from_end = frames - 1 - at;
            if (from_end as usize) < fade_out {
                gain *= ramp(from_end as usize, fade_out);
            }
            if gain != 1.0 {
                for ch in frame.chunks_exact_mut(4) {
                    let v = f32::from_le_bytes([ch[0], ch[1], ch[2], ch[3]]);
                    ch.copy_from_slice(&((v as f64 * gain) as f32).to_le_bytes());
                }
            }
        }
        w.write_all(block).map_err(werr)?;
        done += n as u64;
    }
    w.flush().map_err(werr)
}
