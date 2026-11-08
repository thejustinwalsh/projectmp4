//! Pipes raw RGBA frames into ffmpeg and muxes the original audio alongside.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread::JoinHandle;

use crate::ffi::{Error, Result};
use crate::presets::packs::pid_alive;

/// How much of ffmpeg's stderr is kept for error messages.
const STDERR_TAIL: usize = 8 * 1024;

/// Where the output is going, which decides how many bytes it may cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Target {
    /// No size goal: constant quality (CRF 17).
    #[default]
    None,
    /// Discord Nitro: 500 MB.
    Discord,
    /// X: 512 MB and 140 s.
    X,
    /// YouTube takes what it is given.
    YouTube,
}

impl Target {
    pub fn name(self) -> &'static str {
        match self {
            Target::None => "none",
            Target::Discord => "discord",
            Target::X => "x",
            Target::YouTube => "youtube",
        }
    }

    pub fn parse(s: &str) -> Result<Target> {
        match s {
            "none" => Ok(Target::None),
            "discord" => Ok(Target::Discord),
            "x" => Ok(Target::X),
            "youtube" => Ok(Target::YouTube),
            _ => Err(Error::msg(format!("{s:?} is not a target; expected none, discord, x or youtube"))),
        }
    }
}

/// Fraction of the size cap the video+audio payload may use. Absorbs the
/// measured +3.2% single-pass VBR overshoot with room.
const MARGIN: f64 = 0.92;
/// Below this, 1080p60 of this content falls apart.
const FLOOR_KBPS: f64 = 6000.0;
/// Measured CRF 17 average on a real full track (16.03 Mb/s).
pub const CRF_KBPS: f64 = 16030.0;
/// Budgets above this: CRF is cheaper than the cap, so keep CRF. Derived from
/// `CRF_KBPS`: 16030 * 1.25 = 20037.5, rounded to 20000. That is CRF's measured
/// cost plus 25% headroom for content costlier than the one measured.
const CAP_CRF_AT: f64 = 20000.0;
/// X's free-account duration limit.
pub const X_MAX_SECONDS: f64 = 140.0;
/// The mp4 audio bitrate `ffmpeg_args` asks for.
pub const AUDIO_KBPS: u32 = 256;

/// The kbps `duration_s` of video may spend to fit `target`, once the audio
/// track is paid for. `Ok(None)` is a target with no size cap; `Err` is a
/// target that cannot be met at any bitrate.
fn budget_kbps(target: Target, duration_s: f64, audio_kbps: u32) -> Result<Option<f64>> {
    if !duration_s.is_finite() || duration_s <= 0.0 {
        return Err(Error::msg(format!("cannot size a {duration_s} s render for --target {}", target.name())));
    }
    let cap_bytes = match target {
        Target::None | Target::YouTube => return Ok(None),
        Target::Discord => 500e6,
        Target::X => {
            if duration_s > X_MAX_SECONDS {
                return Err(Error::msg(format!(
                    "--target x allows {X_MAX_SECONDS:.0} s and this render is {duration_s:.1} s; \
                     no bitrate fixes a duration limit. Cut to the best part with --clip drop, \
                     or shorten it with --limit-seconds {X_MAX_SECONDS:.0}"
                )));
            }
            512e6
        }
    };
    let budget = cap_bytes * 8.0 * MARGIN / duration_s / 1000.0 - audio_kbps as f64;
    if budget < FLOOR_KBPS {
        return Err(Error::msg(format!(
            "{duration_s:.0} s will not fit --target {} at an acceptable quality: that leaves {:.1} Mb/s for \
             video, under the {:.0} Mb/s floor for 1080p60. Lower --fps, or use --target youtube",
            target.name(),
            budget / 1000.0,
            FLOOR_KBPS / 1000.0
        )));
    }
    Ok(Some(budget))
}

/// The video bitrate (kbps) that makes `duration_s` of **libx264** output fit
/// `target`. `Ok(None)`: use CRF 17. `Ok(Some(k))`: cap at k kbps. `Err`:
/// impossible.
///
/// Use [`video_bitrate_kbps_fast`] for the `--fast` encoder: the `Ok(None)`
/// here means "CRF 17 already fits", which rests on `CRF_KBPS`, a measurement
/// of libx264 and of nothing else.
pub fn video_bitrate_kbps(target: Target, duration_s: f64, audio_kbps: u32) -> Result<Option<u32>> {
    let Some(budget) = budget_kbps(target, duration_s, audio_kbps)? else {
        return Ok(None);
    };
    if budget >= CAP_CRF_AT {
        return Ok(None);
    }
    // Down, never up: rounding up spends margin we do not have.
    // Never ask for more than CRF 17 would spend: between the CRF cost and
    // CAP_CRF_AT a cap at the full budget would inflate the file.
    Ok(Some(budget.min(CRF_KBPS).floor() as u32))
}

/// The video bitrate (kbps) for the `--fast` encoder, h264_videotoolbox.
///
/// Unlike [`video_bitrate_kbps`] this never answers "constant quality fits".
/// Constant quality on VideoToolbox is `-q:v 65`, whose cost is not `CRF_KBPS`
/// and is not bounded by anything we have measured: on a real 1080p60 track it
/// came out at 109 Mb/s, 6.8x libx264's CRF 17. Taking the `CAP_CRF_AT`
/// shortcut on this encoder produced a 2.4 GB file for `--target discord`,
/// which caps at 500 MB. So a capped target always gets an explicit bitrate
/// here, and only an uncapped one (`none`, `youtube`) gets constant quality.
///
/// The budget is not clamped to `CRF_KBPS` either, for the same reason: that
/// clamp asks for no more than libx264 would have spent, which says nothing
/// about this encoder and would only give away cap headroom.
pub fn video_bitrate_kbps_fast(target: Target, duration_s: f64, audio_kbps: u32) -> Result<Option<u32>> {
    Ok(budget_kbps(target, duration_s, audio_kbps)?.map(|b| b.floor() as u32))
}

pub struct Encoder {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stderr: Option<JoinHandle<Vec<u8>>>,
    frame_len: usize,
    out: PathBuf,
    /// ffmpeg writes here; only a verified success renames it onto `out`.
    part: PathBuf,
}

/// Private staging file next to `out` (same filesystem, so the final rename is
/// atomic), unique per process, thread and call so concurrent renders to one
/// output never share bytes. Only the file this run creates is ever removed.
fn part_path(out: &Path) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let mut name = out.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".part.{}.{:?}.{}",
        std::process::id(),
        std::thread::current().id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    out.with_file_name(name)
}

/// Remove `<out>.part.<pid>.*` files left beside `out` by runs that were killed
/// (SIGKILL, power loss). Only a file whose pid is not a live process is
/// removed, so a concurrent render's staging file is never touched.
fn sweep_orphaned_parts(out: &Path) {
    let Some(name) = out.file_name().and_then(|n| n.to_str()) else { return };
    let prefix = format!("{name}.part.");
    let dir = match out.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let file = e.file_name();
        let Some(rest) = file.to_str().and_then(|f| f.strip_prefix(&prefix)) else { continue };
        let Some(pid) = rest.split('.').next().and_then(|p| p.parse::<u32>().ok()) else { continue };
        if e.file_type().is_ok_and(|t| t.is_file()) && !pid_alive(pid) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The ffmpeg argument vector (without the program name).
///
/// `bitrate_kbps` of `Some(k)` replaces constant quality with a single-pass
/// capped bitrate; `None` is CRF 17 (videotoolbox: `-q:v 65`).
pub fn ffmpeg_args(out: &Path, audio: &Path, w: u32, h: u32, fps: u32, fast: bool, bitrate_kbps: Option<u32>) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    // `-y`: without it, a staging file that already exists makes ffmpeg print
    // "already exists. Exiting." and exit 0, which would pass for success.
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
    let rate = bitrate_kbps.map(|k| {
        let k = k as u64;
        [
            "-b:v".to_string(),
            format!("{k}k"),
            "-maxrate".to_string(),
            format!("{}k", k * 12 / 10),
            "-bufsize".to_string(),
            format!("{}k", k * 24 / 10),
        ]
    });
    if fast {
        // h264_videotoolbox accepts neither -crf nor -preset (it ignores them
        // silently); -q:v is its quality control, and it honours a bitrate.
        for s in ["-c:v", "h264_videotoolbox"] {
            a.push(s.into());
        }
        match &rate {
            Some(r) => a.extend(r.iter().map(OsString::from)),
            None => a.extend(["-q:v", "65"].map(OsString::from)),
        }
    } else {
        for s in ["-c:v", "libx264"] {
            a.push(s.into());
        }
        match &rate {
            Some(r) => a.extend(r.iter().map(OsString::from)),
            None => a.extend(["-crf", "17"].map(OsString::from)),
        }
        for s in ["-preset", "slow"] {
            a.push(s.into());
        }
    }
    for s in ["-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "256k", "-movflags", "+faststart", "-shortest"] {
        a.push(s.into());
    }
    // `.part` gives ffmpeg no muxer to infer, so name it.
    for s in ["-f", "mp4"] {
        a.push(s.into());
    }
    a.push(out.into());
    a
}

impl Encoder {
    pub fn start(out: &Path, audio: &Path, w: u32, h: u32, fps: u32, fast: bool, bitrate_kbps: Option<u32>) -> Result<Encoder> {
        Self::start_with_program("ffmpeg", out, audio, w, h, fps, fast, bitrate_kbps)
    }

    /// `start` with an explicit ffmpeg-compatible program (a stub, in tests).
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_program(
        program: &str,
        out: &Path,
        audio: &Path,
        w: u32,
        h: u32,
        fps: u32,
        fast: bool,
        bitrate_kbps: Option<u32>,
    ) -> Result<Encoder> {
        sweep_orphaned_parts(out);
        Self::start_staged(program, out, &part_path(out), audio, w, h, fps, fast, bitrate_kbps)
    }

    /// `start_with_program` with a caller-chosen staging path (tests seed it).
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn start_staged(
        program: &str,
        out: &Path,
        part: &Path,
        audio: &Path,
        w: u32,
        h: u32,
        fps: u32,
        fast: bool,
        bitrate_kbps: Option<u32>,
    ) -> Result<Encoder> {
        let frame_len = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(4))
            .filter(|&n| n > 0)
            .ok_or_else(|| Error::msg(format!("invalid frame size {w}x{h}")))?;
        // The staging file is deleted before the run and renamed over `out`
        // after it: it must be a distinct sibling of `out`, or we would destroy
        // the previous render / fail the rename / leave the same-filesystem
        // atomicity.
        if part == out || part.parent() != out.parent() {
            return Err(Error::msg(format!(
                "staging path {} must differ from, and sit in the same directory as, output {}",
                part.display(),
                out.display()
            )));
        }
        let part = part.to_path_buf();
        // A pre-existing staging file must never be mistaken for output.
        match std::fs::remove_file(&part) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::msg(format!("cannot clear staging file {}: {e}", part.display()))),
        }
        let mut child = Command::new(program)
            .args(ffmpeg_args(&part, audio, w, h, fps, fast, bitrate_kbps))
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
            frame_len,
            out: out.to_path_buf(),
            part,
        })
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
                // ffmpeg went away; its exit status and stderr say why. Diagnosis
                // never publishes: whatever it left in the `.part` is truncated.
                let why = match self.wait_child() {
                    Ok((status, tail)) => {
                        let _ = std::fs::remove_file(&self.part);
                        if status.success() {
                            ": ffmpeg exited 0 before all frames were written".to_string()
                        } else {
                            format!(": {}", self.exit_error(status, &tail).0)
                        }
                    }
                    Err(e) => format!(": {}", e.0),
                };
                Err(Error::msg(format!("writing frame to ffmpeg failed ({e}){why}")))
            }
        }
    }

    /// The only place the output is published.
    pub fn finish(mut self) -> Result<()> {
        let (status, tail) = self.wait_child()?;
        if status.success() {
            // Exit 0 is not proof of a video (ffmpeg exits 0 after failing to
            // open an existing output). The `.part` is always created fresh,
            // so existence and a non-zero length are enough.
            let shown = self.out.display();
            let verdict = match std::fs::metadata(&self.part) {
                Ok(m) if m.len() > 0 => std::fs::rename(&self.part, &self.out)
                    .map_err(|e| Error::msg(format!("cannot publish {shown}: {e}"))),
                Ok(_) => Err(Error::msg(format!("ffmpeg exited 0 but wrote an empty {shown}"))),
                Err(e) => Err(Error::msg(format!("ffmpeg exited 0 but wrote no {shown}: missing: {e}"))),
            };
            if verdict.is_err() {
                let _ = std::fs::remove_file(&self.part);
            }
            return verdict;
        }
        let _ = std::fs::remove_file(&self.part);
        Err(self.exit_error(status, &tail))
    }

    /// Close stdin, wait for ffmpeg, join the stderr drain. Touches no files.
    fn wait_child(&mut self) -> Result<(std::process::ExitStatus, Vec<u8>)> {
        drop(self.stdin.take());
        let Some(mut child) = self.child.take() else {
            return Err(Error::msg("encoder already closed"));
        };
        let status = child.wait().map_err(|e| Error::msg(format!("waiting for ffmpeg: {e}")))?;
        let tail = self.stderr.take().and_then(|t| t.join().ok()).unwrap_or_default();
        Ok((status, tail))
    }

    fn exit_error(&self, status: std::process::ExitStatus, tail: &[u8]) -> Error {
        Error::msg(format!(
            "ffmpeg writing {} exited with {status}: {}",
            self.out.display(),
            String::from_utf8_lossy(tail).trim()
        ))
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            drop(self.stdin.take());
            let _ = c.kill();
            let _ = c.wait();
        }
        // No-op after a successful rename.
        let _ = std::fs::remove_file(&self.part);
    }
}
