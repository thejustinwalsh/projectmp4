//! Network tests: they clone ~10k presets from GitHub, so they are `#[ignore]`d
//! and do not run in the default `cargo test`. Run with
//! `cargo test --test packs -- --ignored`.

use projectmp4::presets::packs::sources;
use projectmp4::Packs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn cache_dir() -> PathBuf {
    // Outside the repo, shared across runs so only the first test pays for the clone.
    std::env::temp_dir().join("projectmp4-packs-test-cache")
}

/// Populate the shared cache exactly once per test binary; every test waits on
/// this, so none races another's clone/rename on a cold cache.
fn populated() -> Packs {
    static POPULATED: OnceLock<Result<Packs, String>> = OnceLock::new();
    POPULATED
        .get_or_init(|| Packs::ensure(&cache_dir()).map_err(|e| e.to_string()))
        .clone()
        .unwrap_or_else(|e| panic!("populating the pack cache failed: {e}"))
}

/// Removes the file on drop, so a failing assertion cannot leave it behind.
struct RemoveOnDrop(PathBuf);
impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}

fn count_milk(dir: &Path) -> usize {
    let mut n = 0;
    walk(dir, &mut |p| {
        if p.extension().is_some_and(|e| e == "milk") {
            n += 1;
        }
    });
    n
}

fn count_files(dir: &Path) -> usize {
    let mut n = 0;
    walk(dir, &mut |_| n += 1);
    n
}

fn head(clone: &Path) -> String {
    let out = Command::new("git").arg("-C").arg(clone).args(["rev-parse", "HEAD"]).output().unwrap();
    assert!(out.status.success(), "git rev-parse HEAD failed in {}: {}", clone.display(), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn pin(name: &str) -> String {
    sources().unwrap().into_iter().find(|s| s.name == name).unwrap().commit
}

/// The checked-out commit of each clone equals its pin.
fn assert_pinned(cache: &Path) {
    for name in ["presets-cream-of-the-crop", "presets-projectm-classic", "presets-milkdrop-texture-pack"] {
        assert_eq!(head(&cache.join(name)), pin(name), "{name} is not at its pinned commit");
    }
}

fn assert_counts(p: &Packs) {
    assert_eq!(count_milk(&p.cotc), 9795);
    assert_eq!(count_milk(&p.bltc()), 413);
    assert_eq!(count_milk(&p.stock()), 557);
    assert_eq!(count_files(&p.textures), 67);
    assert!(p.textures.join("Image415.jpg").is_file());
    assert!(p.textures.ends_with("presets-milkdrop-texture-pack/textures"));
}

#[test]
#[ignore = "clones ~10k presets from GitHub (network)"]
fn fetches_packs_and_finds_the_curated_subsets() {
    let cache = cache_dir();
    let p = populated();
    assert_pinned(&cache);
    assert_counts(&p);
}

#[test]
#[ignore = "network on first run (shares the clone cache)"]
fn second_call_is_idempotent() {
    let cache = cache_dir();
    let p = populated();
    let before: Vec<String> = ["presets-cream-of-the-crop", "presets-projectm-classic", "presets-milkdrop-texture-pack"]
        .iter()
        .map(|n| head(&cache.join(n)))
        .collect();
    // A sentinel inside a clone dir survives only if the second call does not re-clone.
    let sentinel = p.cotc.join(".git").join("idempotence-sentinel");
    let _ = std::fs::remove_file(&sentinel); // stale one from an aborted run
    let _cleanup = RemoveOnDrop(sentinel.clone());
    std::fs::write(&sentinel, "x").unwrap();

    let p2 = Packs::ensure(&cache).unwrap();

    assert!(sentinel.exists(), "second ensure re-cloned instead of reusing the pack");
    assert_pinned(&cache);
    let after: Vec<String> = ["presets-cream-of-the-crop", "presets-projectm-classic", "presets-milkdrop-texture-pack"]
        .iter()
        .map(|n| head(&cache.join(n)))
        .collect();
    assert_eq!(before, after);
    assert_counts(&p2);
}
