//! Pipes raw RGBA frames into ffmpeg and muxes the original audio alongside.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread::JoinHandle;

use crate::ffi::{Error, Result};

/// How much of ffmpeg's stderr is kept for error messages.
const STDERR_TAIL: usize = 8 * 1024;

pub struct Encoder {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    frame_len: usize,
    out: String,
}

/// The ffmpeg argument vector (without the program name).
pub fn ffmpeg_args(out: &Path, audio: &Path, w: u32, h: u32, fps: u32, fast: bool) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    // `-y`: without it ffmpeg prompts on an existing output and reads the
    // answer from stdin, which is our frame pipe.
    // `-nostats -loglevel warning`: keep stderr volume low (it is also drained
    // on a thread, so this is belt and braces).
    for s in ["-y", "-nostats", "-loglevel", "warning", "-f", "rawvideo", "-pix_fmt", "rgba", "-s"] {
        a.push(s.into());
    }
    a.push(format!("{w}x{h}").into());
    a.push("-r".into());
    a.push(fps.to_string().into());
    for s in ["-i", "-", "-i"] {
        a.push(s.into());
    }
    a.push(audio.into());
    for s in ["-map", "0:v", "-map", "1:a"] {
        a.push(s.into());
    }
    if fast {
        // h264_videotoolbox accepts neither -crf nor -preset (it ignores them
        // silently); -q:v is its quality control.
        for s in ["-c:v", "h264_videotoolbox", "-q:v", "65"] {
            a.push(s.into());
        }
    } else {
        for s in ["-c:v", "libx264", "-crf", "17", "-preset", "slow"] {
            a.push(s.into());
        }
    }
    for s in ["-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "256k", "-movflags", "+faststart", "-shortest"] {
        a.push(s.into());
    }
    a.push(out.into());
    a
}

impl Encoder {
    pub fn start(out: &Path, audio: &Path, w: u32, h: u32, fps: u32, fast: bool) -> Result<Encoder> {
        let mut child = Command::new("ffmpeg")
            .args(ffmpeg_args(out, audio, w, h, fps, fast))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::msg(format!("cannot start ffmpeg: {e}")))?;
        let stdin = child.stdin.take();
        let mut err = child.stderr.take().expect("stderr piped");
        // Drain continuously: a full stderr pipe would block ffmpeg while we
        // block writing frames.
        let stderr = std::thread::spawn(move || {
            let mut tail: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match err.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        tail.extend_from_slice(&buf[..n]);
                        if tail.len() > STDERR_TAIL {
                            tail.drain(..tail.len() - STDERR_TAIL);
                        }
                    }
                }
            }
            tail
        });
        Ok(Encoder {
            child: Some(child),
            stdin,
            stderr: Some(stderr),
            frame_len: w as usize * h as usize * 4,
            out: out.display().to_string(),
        })
    }

    /// OS process id of the ffmpeg child (for tests that kill it).
    pub fn pid(&self) -> u32 {
        self.child.as_ref().map_or(0, |c| c.id())
    }

    pub fn write_frame(&mut self, rgba: &[u8]) -> Result<()> {
        if rgba.len() != self.frame_len {
            return Err(Error::msg(format!(
                "frame is {} bytes, expected {}",
                rgba.len(),
                self.frame_len
            )));
        }
        let res = match self.stdin.as_mut() {
            Some(s) => s.write_all(rgba),
            None => return Err(Error::msg("encoder already closed")),
        };
        match res {
            Ok(()) => Ok(()),
            Err(e) => {
                // ffmpeg went away; its exit status and stderr say why.
                let why = self.reap().err().map_or_else(String::new, |e| format!(": {}", e.0));
                Err(Error::msg(format!("writing frame to ffmpeg failed ({e}){why}")))
            }
        }
    }

    pub fn finish(mut self) -> Result<()> {
        self.reap()
    }

    /// Close stdin, wait for ffmpeg, join the stderr drain; Err on non-zero exit.
    fn reap(&mut self) -> Result<()> {
        drop(self.stdin.take());
        let Some(mut child) = self.child.take() else {
            return Err(Error::msg("encoder already closed"));
        };
        let status = child.wait().map_err(|e| Error::msg(format!("waiting for ffmpeg: {e}")))?;
        let tail = self.stderr.take().and_then(|t| t.join().ok()).unwrap_or_default();
        if status.success() {
            return Ok(());
        }
        Err(Error::msg(format!(
            "ffmpeg writing {} exited with {status}: {}",
            self.out,
            String::from_utf8_lossy(&tail).trim()
        )))
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            drop(self.stdin.take());
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
