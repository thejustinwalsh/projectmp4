//! Durable analysis units. Neural inference and ranking remain atomic stages.
use crate::{features, BeatThis, RtenRuntime};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const CONFIG: &[u8] = b"deck-analysis-checkpoint-v1;codec-48k-opus-v1;beat-this-1.1.0-small1-rten;dj-chroma-a440-48k-v1;pymusiclooper-78313f8-dj-v1;features-chunk-256";
const CHUNK: usize = 256;

#[derive(Default)]
pub struct Schedule {
    /// Cooperative wall-clock budget. Atomic stages may exceed this deadline.
    pub slice_ms: u64,
    /// Optional stop sentinel, checked before each work unit and mel push.
    pub cancel_file: Option<PathBuf>,
    /// Deterministic unit budget, useful for background scheduling and tests.
    pub max_units: Option<usize>,
}
#[derive(Debug)]
pub struct Yielded(pub &'static str);
impl std::fmt::Display for Yielded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Analysis yielded: {}", self.0)
    }
}
impl Error for Yielded {}
struct Budget<'a> {
    schedule: &'a Schedule,
    started: Instant,
    units: usize,
}
impl Budget<'_> {
    fn cancellation(&self) -> Result<()> {
        if self
            .schedule
            .cancel_file
            .as_ref()
            .is_some_and(|path| path.exists())
        {
            return Err(Box::new(Yielded("cancelled")));
        }
        Ok(())
    }
    fn before(&self) -> Result<()> {
        self.cancellation()?;
        if self.schedule.max_units.is_some_and(|max| self.units >= max)
            || (self.units > 0
                && self.schedule.slice_ms > 0
                && self.started.elapsed() >= Duration::from_millis(self.schedule.slice_ms))
        {
            return Err(Box::new(Yielded("slice exhausted")));
        }
        // Load changes are sampled between actual work units. There is no
        // concurrent analysis thread pool; leave breathing room when oversubscribed.
        #[cfg(unix)]
        unsafe {
            let mut load = 0.;
            if getloadavg(&mut load, 1) == 1 {
                let cores = std::thread::available_parallelism().map_or(1, usize::from) as f64;
                if load > cores * 0.75 {
                    std::thread::sleep(Duration::from_millis(
                        ((load / cores - 0.75) * 20.).clamp(1., 100.) as u64,
                    ));
                    self.cancellation()?;
                }
            }
        }
        Ok(())
    }
}
#[cfg(unix)]
extern "C" {
    fn getloadavg(values: *mut f64, count: i32) -> i32;
    fn flock(fd: i32, operation: i32) -> i32;
}

#[cfg(windows)]
mod windows_lock {
    use std::{ffi::c_void, fs::File, io, os::windows::io::AsRawHandle};

    // Win32 OVERLAPPED: its offset/pointer union occupies eight bytes on both
    // 32- and 64-bit Windows. Synchronous File handles need no event object.
    #[repr(C)]
    #[derive(Default)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut c_void,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LockFileEx(
            file: *mut c_void,
            flags: u32,
            reserved: u32,
            length_low: u32,
            length_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn UnlockFileEx(
            file: *mut c_void,
            reserved: u32,
            length_low: u32,
            length_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }
    pub(super) fn acquire(file: &File) -> io::Result<()> {
        const LOCKFILE_FAIL_IMMEDIATELY: u32 = 1;
        const LOCKFILE_EXCLUSIVE_LOCK: u32 = 2;
        let mut overlapped = Overlapped::default();
        // Lock byte zero even when worker.lock is empty. Win32 permits locking
        // beyond EOF. The synchronous, nonblocking call does not retain this pointer.
        if unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_FAIL_IMMEDIATELY | LOCKFILE_EXCLUSIVE_LOCK,
                0,
                1,
                0,
                &mut overlapped,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    pub(super) fn release(file: &File) {
        let mut overlapped = Overlapped::default();
        // Closing File is the fallback if explicit unlock fails. Both normal
        // closure and process termination release this kernel-owned range lock.
        unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) };
    }
    #[test]
    fn overlapped_matches_windows_abi() {
        use std::mem::{offset_of, size_of};
        assert_eq!(offset_of!(Overlapped, offset), 2 * size_of::<usize>());
        assert_eq!(offset_of!(Overlapped, event), 2 * size_of::<usize>() + 8);
        assert_eq!(
            size_of::<Overlapped>(),
            if cfg!(target_pointer_width = "64") {
                32
            } else {
                20
            }
        );
    }
}

// The kernel releases this advisory lock on kill/crash. A persistent lock file
// is intentional; deleting it would let two processes lock different inodes.
struct Lock(File);
impl Lock {
    fn acquire(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
                return Err("Analysis checkpoint is already in use".into());
            }
        }
        #[cfg(windows)]
        {
            if let Err(error) = windows_lock::acquire(&file) {
                if error.raw_os_error() == Some(33) {
                    // ERROR_LOCK_VIOLATION
                    return Err("Analysis checkpoint is already in use".into());
                }
                return Err(error.into());
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            return Err("Durable analysis locking requires Unix or Windows".into());
        }
        Ok(Self(file))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.sync_all();
        #[cfg(windows)]
        windows_lock::release(&self.0);
    }
}

fn digest_file(path: &Path, hasher: &mut Sha256) -> Result<()> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    hasher.update(len.to_le_bytes());
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(())
}
fn identity(models: &Path, input: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(CONFIG);
    digest_file(input, &mut hash)?;
    for name in ["mel_spectrogram.onnx", "beat_this_small.onnx"] {
        digest_file(&models.join(name), &mut hash)?;
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn save(path: &Path, payload: &[u8]) -> Result<()> {
    let temporary = path.with_extension("pending");
    let mut file = File::create(&temporary)?;
    file.write_all(&Sha256::digest(payload))?;
    file.write_all(payload)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    #[cfg(unix)]
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}
fn load(path: &Path) -> Result<Option<Vec<u8>>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if bytes.len() < 32 || Sha256::digest(&bytes[32..])[..] != bytes[..32] {
        // A damaged artifact is recomputed. Never use partial data.
        return Ok(None);
    }
    Ok(Some(bytes[32..].to_vec()))
}
fn write_progress(dir: &Path, stage: &str, complete: usize, total: usize) -> Result<()> {
    save(
        &dir.join("progress.json"),
        &serde_json::to_vec(
            &json!({"version":1,"stage":stage,"completed":complete,"total":total}),
        )?,
    )
}
fn encode_features(value: &features::Features) -> Vec<u8> {
    value
        .power
        .iter()
        .chain(&value.chroma)
        .flat_map(|v| v.to_le_bytes())
        .collect()
}
fn decode_features(bytes: &[u8], frames: usize) -> Option<features::Features> {
    if bytes.len() != frames * 13 * 8 {
        return None;
    }
    let values: Vec<_> = bytes
        .chunks_exact(8)
        .map(|v| f64::from_le_bytes(v.try_into().unwrap()))
        .collect();
    if values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    Some(features::Features {
        power: values[..frames].to_vec(),
        chroma: values[frames..].to_vec(),
    })
}

/// Reuse completed units after a process restart. Returns `Yielded` for a
/// cooperative stop; call again with the same content and checkpoint directory.
pub fn analyze_resumable(
    models: &Path,
    input: &Path,
    checkpoint: &Path,
    schedule: &Schedule,
) -> Result<Value> {
    let started = Instant::now();
    let mut budget = Budget {
        schedule,
        started,
        units: 0,
    };
    budget.cancellation()?;
    let key = identity(models, input)?;
    let dir = checkpoint.join(&key);
    fs::create_dir_all(&dir)?;
    let _lock = Lock::acquire(&dir.join("worker.lock"))?;
    if let Some(bytes) = load(&dir.join("result.json"))? {
        if let Ok(value) = serde_json::from_slice(&bytes) {
            return Ok(value);
        }
    }
    // Hash again at the end: replacing the input/model while work is running
    // must never publish a result under the old identity.
    let pcm = if let Some(bytes) = load(&dir.join("pcm.f32"))? {
        if bytes.len() % 4 != 0 || bytes.is_empty() {
            return Err("Invalid PCM checkpoint".into());
        }
        bytes
            .chunks_exact(4)
            .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
            .collect::<Vec<_>>()
    } else {
        budget.before()?;
        write_progress(&dir, "decode", 0, 1)?;
        let pcm = deck_media_codec::decode_mono(input)?;
        save(
            &dir.join("pcm.f32"),
            &pcm.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>(),
        )?;
        budget.units += 1;
        pcm
    };
    if pcm.iter().fold(0_f32, |max, sample| max.max(sample.abs())) < 1e-6 {
        return Err("Silent audio has no usable beat/loop suggestions".into());
    }
    // Compute cheap independent features first: even tiny slices make durable
    // progress before entering the indivisible neural inference stage.
    let total = pcm.len() / features::HOP + 1;
    let mut extracted = features::Features {
        chroma: Vec::with_capacity(total * 12),
        power: Vec::with_capacity(total),
    };
    for start in (0..total).step_by(CHUNK) {
        let end = (start + CHUNK).min(total);
        let path = dir.join(format!("features-{start:08}.bin"));
        let cached = load(&path)?.and_then(|bytes| decode_features(&bytes, end - start));
        let chunk = if let Some(chunk) = cached {
            chunk
        } else {
            budget.before()?;
            let chunk = features::extract_range(&pcm, start, end);
            save(&path, &encode_features(&chunk))?;
            budget.units += 1;
            write_progress(&dir, "features", end, total)?;
            chunk
        };
        extracted.chroma.extend(chunk.chroma);
        extracted.power.extend(chunk.power);
    }
    let beats = if let Some(bytes) = load(&dir.join("beats.json"))? {
        serde_json::from_slice::<Value>(&bytes)?
    } else {
        budget.before()?;
        write_progress(&dir, "beats", 0, 1)?;
        let mut tracker = BeatThis::new(
            &RtenRuntime,
            &models.join("mel_spectrogram.onnx"),
            &models.join("beat_this_small.onnx"),
        )?;
        let mut stream = tracker.stream(48000)?;
        for chunk in pcm.chunks(48000) {
            // Cancellation can discard this in-memory mel state. Slice budgets
            // are NOT checked here: repeated short slices would starve inference.
            budget.cancellation()?;
            stream.push(chunk)?;
        }
        budget.cancellation()?;
        let result = stream.finish()?;
        let value = json!({"bpm":beat_this::calculate_bpm(&result).unwrap_or(0.),"beats":result.beats,"downbeats":result.downbeats});
        save(&dir.join("beats.json"), &serde_json::to_vec(&value)?)?;
        budget.units += 1;
        value
    };
    budget.before()?;
    write_progress(&dir, "rank", 0, 1)?;
    let beat_times = serde_json::from_value::<Vec<f32>>(beats["beats"].clone())?;
    let downbeats = serde_json::from_value::<Vec<f32>>(beats["downbeats"].clone())?;
    let value = crate::assemble(
        pcm.len() as f64 / 48000.,
        beats["bpm"].as_f64().ok_or("Invalid BPM checkpoint")? as f32,
        &beat_times,
        &downbeats,
        extracted,
        started,
    )?;
    budget.cancellation()?;
    if identity(models, input)? != key {
        return Err("Analysis input or models changed while processing".into());
    }
    save(&dir.join("result.json"), &serde_json::to_vec(&value)?)?;
    write_progress(&dir, "complete", 1, 1)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn damaged_artifact_is_rejected_and_atomic_replacement_works() {
        let dir = std::env::temp_dir().join(format!("deck-artifact-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unit.bin");
        save(&path, b"complete").unwrap();
        assert_eq!(load(&path).unwrap().unwrap(), b"complete");
        fs::write(&path, b"torn").unwrap();
        assert!(load(&path).unwrap().is_none());
        save(&path, b"replacement").unwrap();
        assert_eq!(load(&path).unwrap().unwrap(), b"replacement");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn content_and_model_changes_invalidate_identity() {
        let dir = std::env::temp_dir().join(format!("deck-identity-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let input = dir.join("source.mp4");
        fs::write(&input, b"audio-a").unwrap();
        fs::write(dir.join("mel_spectrogram.onnx"), b"mel-a").unwrap();
        fs::write(dir.join("beat_this_small.onnx"), b"beat-a").unwrap();
        let first = identity(&dir, &input).unwrap();
        fs::write(&input, b"audio-b").unwrap();
        let second = identity(&dir, &input).unwrap();
        assert_ne!(first, second);
        fs::write(dir.join("beat_this_small.onnx"), b"beat-b").unwrap();
        assert_ne!(second, identity(&dir, &input).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(any(unix, windows))]
    #[test]
    fn lock_excludes_second_worker_and_releases_on_close() {
        let path = std::env::temp_dir().join(format!("deck-lock-test-{}", std::process::id()));
        let first = Lock::acquire(&path).unwrap();
        assert!(Lock::acquire(&path).is_err());
        drop(first);
        assert!(Lock::acquire(&path).is_ok());
        fs::remove_file(path).unwrap();
    }
    #[cfg(any(unix, windows))]
    #[test]
    fn lock_child_worker() {
        let Some(path) = std::env::var_os("DECK_ANALYSIS_TEST_CHILD_LOCK") else {
            return;
        };
        let path = PathBuf::from(path);
        let _lock = Lock::acquire(&path).unwrap();
        fs::write(path.with_extension("ready"), b"locked").unwrap();
        loop {
            std::thread::park();
        }
    }
    #[cfg(any(unix, windows))]
    #[test]
    fn lock_releases_after_worker_process_is_killed() {
        let path =
            std::env::temp_dir().join(format!("deck-killed-lock-test-{}", std::process::id()));
        let ready = path.with_extension("ready");
        let _ = fs::remove_file(&ready);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "scheduling::tests::lock_child_worker",
                "--nocapture",
            ])
            .env("DECK_ANALYSIS_TEST_CHILD_LOCK", &path)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() && Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !ready.exists() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Child worker did not acquire its checkpoint lock");
        }
        let excluded = Lock::acquire(&path).is_err();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            excluded,
            "Another process must not share the checkpoint lock"
        );
        // Windows may release a terminated process's byte-range locks shortly
        // after process termination is reported. Require eventual, bounded release.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match Lock::acquire(&path) {
                Ok(lock) => {
                    drop(lock);
                    break;
                }
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("Killed worker's kernel lock was not released: {error}"),
            }
        }
        fs::remove_file(path).unwrap();
        fs::remove_file(ready).unwrap();
    }
    #[test]
    fn feature_artifact_preserves_exact_values() {
        let value = features::extract(&vec![0.25; 4000]);
        let restored = decode_features(&encode_features(&value), value.power.len()).unwrap();
        assert_eq!(value.chroma, restored.chroma);
        assert_eq!(value.power, restored.power);
    }
}
