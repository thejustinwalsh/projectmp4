//! Real Opus/model integration test. No audio or model assets are embedded.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn command(models: &Path, input: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_deck-track-analysis"));
    command.arg(models).arg(input);
    command
}
fn artifact(dir: &Path, name: &str) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join(name))
        .find(|path| path.is_file())
}
#[test]
#[ignore = "requires DECK_ANALYSIS_FIXTURE with actual 48 kHz Opus-in-MP4 audio and local models"]
fn real_audio_crash_resume_matches_uninterrupted_analysis() {
    let input = PathBuf::from(std::env::var_os("DECK_ANALYSIS_FIXTURE").expect("fixture path"));
    let models = Path::new(env!("CARGO_MANIFEST_DIR")).join("models");
    let dir = std::env::temp_dir().join(format!("deck-resume-integration-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let baseline = command(&models, &input).output().unwrap();
    assert!(
        baseline.status.success(),
        "{}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    let mut baseline: serde_json::Value = serde_json::from_slice(&baseline.stdout).unwrap();
    // One real unit: decode is retained; the next invocation starts features.
    let first = command(&models, &input)
        .arg("--checkpoint-dir")
        .arg(&dir)
        .args(["--max-units", "1"])
        .output()
        .unwrap();
    assert_eq!(first.status.code(), Some(75));
    assert!(artifact(&dir, "pcm.f32").is_some());
    assert!(artifact(&dir, "features-00000000.bin").is_none());
    let second = command(&models, &input)
        .arg("--checkpoint-dir")
        .arg(&dir)
        .args(["--max-units", "1"])
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(75));
    let kept = artifact(&dir, "features-00000000.bin").unwrap();
    let original = fs::read(&kept).unwrap();
    let modified = fs::metadata(&kept).unwrap().modified().unwrap();
    // Kill during active feature/neural work. A fake torn pending file must
    // never count as a completed unit, even after the process is restarted.
    fs::write(
        kept.parent().unwrap().join("features-99999999.pending"),
        b"torn",
    )
    .unwrap();
    let mut child = command(&models, &input)
        .arg("--checkpoint-dir")
        .arg(&dir)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if artifact(&dir, "features-00002560.bin").is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for real feature work"
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "worker finished before interruption"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(fs::read(&kept).unwrap(), original);
    let cancel = dir.join("cancel");
    fs::write(&cancel, b"stop").unwrap();
    let stopped = command(&models, &input)
        .arg("--checkpoint-dir")
        .arg(&dir)
        .arg("--cancel-file")
        .arg(&cancel)
        .output()
        .unwrap();
    assert_eq!(stopped.status.code(), Some(75));
    fs::remove_file(cancel).unwrap();
    // Resume the killed CLI worker through the actual additive C ABI, one
    // durable unit per call. This also proves the two entry points interoperate.
    let c_models = std::ffi::CString::new(models.to_str().unwrap()).unwrap();
    let c_input = std::ffi::CString::new(input.to_str().unwrap()).unwrap();
    let c_checkpoint = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
    let mut yielded = 0;
    let abi_result = loop {
        let mut output = vec![0_u8; 1024 * 1024];
        let mut written = 0;
        let mut error = [0_u8; 512];
        let status = unsafe {
            deck_track_analysis::deck_analyze_slice(
                c_models.as_ptr(),
                c_input.as_ptr(),
                c_checkpoint.as_ptr(),
                10,
                output.as_mut_ptr(),
                output.len(),
                &mut written,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        assert!(
            status == 0 || status == 1,
            "C ABI failed: {}",
            String::from_utf8_lossy(&error)
        );
        let value: serde_json::Value = serde_json::from_slice(&output[..written]).unwrap();
        if status == 0 {
            break value;
        }
        assert_eq!(value["status"], "yielded");
        yielded += 1;
        assert!(yielded < 1000, "C ABI failed to finish");
    };
    assert!(yielded > 1, "expected actual resumable ABI units");
    let beat_artifact = artifact(&dir, "beats.json").unwrap();
    let beat_modified = fs::metadata(&beat_artifact).unwrap().modified().unwrap();
    let output = dir.join("analysis.json");
    let resumed = command(&models, &input)
        .arg("--checkpoint-dir")
        .arg(&dir)
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let mut result: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(result, abi_result);
    assert_eq!(
        fs::metadata(beat_artifact).unwrap().modified().unwrap(),
        beat_modified,
        "completed neural stage was recomputed"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(output).unwrap()).unwrap(),
        result
    );
    assert_eq!(
        fs::metadata(&kept).unwrap().modified().unwrap(),
        modified,
        "completed chunk was recomputed"
    );
    baseline.as_object_mut().unwrap().remove("elapsedMs");
    result.as_object_mut().unwrap().remove("elapsedMs");
    assert_eq!(baseline, result, "resumption changed musical results");
    // Completed result can be reused without permitting any new work units.
    let cached = command(&models, &input)
        .arg("--checkpoint-dir")
        .arg(&dir)
        .args(["--max-units", "0"])
        .output()
        .unwrap();
    assert!(cached.status.success());
    // A source content change gets a distinct identity, even with the same
    // filename and length. No decode is allowed for this identity test.
    let altered = dir.join("altered.mp4");
    let mut bytes = fs::read(&input).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&altered, bytes).unwrap();
    let changed = command(&models, &altered)
        .arg("--checkpoint-dir")
        .arg(&dir)
        .args(["--max-units", "0"])
        .output()
        .unwrap();
    assert_eq!(
        changed.status.code(),
        Some(75),
        "changed input reused old result"
    );
    eprintln!(
        "real fixture: {} seconds, {} beats, {} downbeats, {} loops; kill/resume exact",
        result["duration"],
        result["beats"].as_array().unwrap().len(),
        result["downbeats"].as_array().unwrap().len(),
        result["loops"].as_array().unwrap().len()
    );
    fs::remove_dir_all(dir).unwrap();
}
