//! Regression guard for the temp-dir leak: test suites must not create
//! directories they cannot remove.
#[path = "common/tmp.rs"]
mod tmp;
use tmp::Tmp;

#[test]
fn tmp_is_removed_on_drop() {
    let t = Tmp::new("guard-drop");
    let dir = t.to_path_buf();
    std::fs::write(dir.join("f"), b"x").unwrap();
    assert!(dir.exists());
    drop(t);
    assert!(!dir.exists(), "Tmp left {} behind", dir.display());
}

#[test]
fn tmp_is_removed_when_the_test_panics() {
    let seen = std::sync::Mutex::new(None);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let t = Tmp::new("guard-panic");
        *seen.lock().unwrap() = Some(t.to_path_buf());
        panic!("simulated test failure");
    }));
    assert!(r.is_err());
    let dir = seen.lock().unwrap().take().unwrap();
    assert!(!dir.exists(), "Tmp left {} behind after a panic", dir.display());
}

/// No suite may build its own `projectmp4-*` directory in the system temp dir.
/// Use `tests/common/tmp.rs` (`Tmp`, `normalised`) instead. Suites that clean up
/// by their own RAII guard, or use one fixed shared directory, are listed.
#[test]
fn suites_do_not_create_unguarded_system_temp_dirs() {
    const ALLOWED: &[&str] = &["render_e2e.rs", "encode.rs", "packs.rs", "rank.rs", "gl_overlay.rs", "tmp_guard.rs"];
    let mut offenders = Vec::new();
    for e in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests")).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        if p.extension().is_some_and(|x| x == "rs") && !ALLOWED.contains(&name.as_str()) {
            if std::fs::read_to_string(&p).unwrap().contains("temp_dir()") {
                offenders.push(name);
            }
        }
    }
    assert!(offenders.is_empty(), "use tests/common/tmp.rs instead of std::env::temp_dir() in: {offenders:?}");
}
