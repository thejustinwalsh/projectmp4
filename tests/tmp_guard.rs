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
    // The panic must be the simulated one, not `Tmp::new` failing before the guard existed.
    let payload = r.unwrap_err();
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"simulated test failure"));
    let dir = seen.lock().unwrap().take().unwrap();
    assert!(!dir.exists(), "Tmp left {} behind after a panic", dir.display());
}

/// Nothing under `tests/` or `src/` may create a directory it cannot remove: no system
/// temp dir, no `/tmp`, no `$TMPDIR`, no `CARGO_TARGET_TMPDIR`, except in the files below.
/// Everything else uses `tests/common/tmp.rs` (`Tmp`, `normalised`).
///
/// The scan is textual, so it is a tripwire rather than a proof: it catches aliased imports
/// (`temp_dir as t`), hardcoded paths and env reads, in every `.rs` file under both trees.
const TOKENS: &[&str] = &["temp_dir", "\"/tmp", "\"/private/tmp", "\"/var/folders", "TMPDIR"];

/// Path (relative to the manifest dir) and why it is allowed.
const ALLOWED: &[(&str, &str)] = &[
    ("tests/common/tmp.rs", "the shared guard itself: owns CARGO_TARGET_TMPDIR"),
    ("tests/packs.rs", "deliberately persistent clone cache, so only the first run pays for the network"),
    ("tests/rank.rs", "reuses the persistent clone cache of tests/packs.rs by name"),
    ("src/presets/packs.rs", "unit-test TmpDir removes itself in Drop"),
    ("src/presets/manifest.rs", "removes its file before any assertion runs"),
    ("src/render/plan.rs", "production TempDir, removed in Drop"),
    // This file names every token in order to scan for it.
    ("tests/tmp_guard.rs", "the scanner"),
];

fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn offending_tokens(source: &str) -> Vec<&'static str> {
    TOKENS.iter().copied().filter(|t| source.contains(t)).collect()
}

#[test]
fn the_scanner_sees_aliases_and_hardcoded_paths() {
    assert!(!offending_tokens("use std::env::temp_dir as t; t()").is_empty());
    assert!(!offending_tokens("let d = PathBuf::from(\"/tmp/projectmp4-x\");").is_empty());
    assert!(!offending_tokens("env!(\"CARGO_TARGET_TMPDIR\")").is_empty());
    assert!(offending_tokens("let d = Tmp::new(\"x\");").is_empty());
}

#[test]
fn suites_do_not_create_unguarded_temp_dirs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rs_files(&root.join("tests"), &mut files);
    rs_files(&root.join("src"), &mut files);
    assert!(files.len() > 20, "scanned only {} files: wrong root?", files.len());
    let mut offenders = Vec::new();
    for p in files {
        let rel = p.strip_prefix(root).unwrap().to_str().unwrap().to_string();
        if ALLOWED.iter().any(|(a, _)| *a == rel) {
            continue;
        }
        let hits = offending_tokens(&std::fs::read_to_string(&p).unwrap());
        if !hits.is_empty() {
            offenders.push((rel, hits));
        }
    }
    assert!(offenders.is_empty(), "use tests/common/tmp.rs (Tmp) instead of a raw temp path in: {offenders:?}");
    for (a, why) in ALLOWED {
        assert!(root.join(a).exists(), "stale allowlist entry {a} ({why})");
    }
}
