//! Fetch the pinned MilkDrop preset packs into a cache directory.
//!
//! The pack contents are third-party data: they are cloned, never executed.

use crate::ffi::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const COTC: &str = "presets-cream-of-the-crop";
const CLASSIC: &str = "presets-projectm-classic";
const TEXTURES: &str = "presets-milkdrop-texture-pack";

const SOURCES_TOML: &str = include_str!("../../presets/sources.toml");

/// One pinned repository from `presets/sources.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub repo: String,
    pub commit: String,
}

#[derive(Debug, Clone)]
pub struct Packs {
    /// Clone root of `presets-cream-of-the-crop`.
    pub cotc: PathBuf,
    /// Clone root of `presets-projectm-classic`.
    pub classic: PathBuf,
    /// `<clone>/textures` of the texture pack (the libprojectM texture search path).
    pub textures: PathBuf,
}

impl Packs {
    /// Clone each pinned pack under `cache` if it is not already at its pin.
    /// Idempotent: a pack whose `HEAD` already equals the pin is left untouched.
    pub fn ensure(cache: &Path) -> Result<Packs> {
        Self::ensure_from(cache, &sources()?)
    }

    /// `ensure` over explicit entries (the seam that makes the entry check testable).
    /// Every pack this type exposes must have an entry; a missing one is an error.
    pub(crate) fn ensure_from(cache: &Path, entries: &[Source]) -> Result<Packs> {
        for name in [COTC, CLASSIC, TEXTURES] {
            if !entries.iter().any(|s| s.name == name) {
                return Err(Error::msg(format!("sources.toml has no entry for {name}")));
            }
        }
        std::fs::create_dir_all(cache)
            .map_err(|e| Error::msg(format!("create {}: {e}", cache.display())))?;
        sweep_orphaned_parts(cache);
        for s in entries {
            ensure_source(cache, s)?;
        }
        let dir = |name: &str| {
            let s = entries.iter().find(|s| s.name == name).expect("checked above");
            cache.join(&s.name)
        };
        Ok(Packs {
            cotc: dir(COTC),
            classic: dir(CLASSIC),
            textures: dir(TEXTURES).join("textures"),
        })
    }

    pub fn bltc(&self) -> PathBuf {
        self.classic.join("bltc201")
    }

    pub fn stock(&self) -> PathBuf {
        self.classic.join("stock")
    }
}

/// Platform user-cache location: `$XDG_CACHE_HOME/projectmp4`, else
/// `~/Library/Caches/projectmp4` on macOS, else `~/.cache/projectmp4`.
pub fn default_cache_dir() -> Result<PathBuf> {
    let xdg = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).filter(|p| p.is_absolute());
    let base = match xdg {
        Some(p) => p,
        None => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| Error::msg("HOME is not set; cannot locate a cache directory"))?;
            if cfg!(target_os = "macos") {
                home.join("Library").join("Caches")
            } else {
                home.join(".cache")
            }
        }
    };
    Ok(base.join("projectmp4").join("presets"))
}

/// The pinned sources, parsed from the embedded `presets/sources.toml`.
pub fn sources() -> Result<Vec<Source>> {
    parse_sources(SOURCES_TOML)
}

fn parse_sources(text: &str) -> Result<Vec<Source>> {
    let mut out: Vec<Source> = Vec::new();
    let mut cur: Option<[Option<String>; 3]> = None;
    let flush = |cur: &mut Option<[Option<String>; 3]>, out: &mut Vec<Source>| -> Result<()> {
        if let Some([n, r, c]) = cur.take() {
            match (n, r, c) {
                (Some(name), Some(repo), Some(commit)) => out.push(Source { name, repo, commit }),
                _ => return Err(Error::msg("sources.toml: [[source]] needs name, repo and commit")),
            }
        }
        Ok(())
    };
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if line == "[[source]]" {
            flush(&mut cur, &mut out)?;
            cur = Some([None, None, None]);
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| Error::msg(format!("sources.toml: bad line {line:?}")))?;
        let v = v.trim().trim_matches('"').to_string();
        let slot = cur.as_mut().ok_or_else(|| Error::msg("sources.toml: key outside [[source]]"))?;
        match k.trim() {
            "name" => slot[0] = Some(v),
            "repo" => slot[1] = Some(v),
            "commit" => slot[2] = Some(v),
            other => return Err(Error::msg(format!("sources.toml: unknown key {other:?}"))),
        }
    }
    flush(&mut cur, &mut out)?;
    for s in &out {
        if s.commit.len() != 40 || !s.commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::msg(format!("sources.toml: {} commit is not a full sha", s.name)));
        }
    }
    Ok(out)
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    // Untrusted third-party repositories: no hook or config from any source may run.
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|e| Error::msg(format!("failed to run git: {e}")))?;
    if !out.status.success() {
        return Err(Error::msg(format!(
            "git {} failed ({}) in {}: {}",
            args.join(" "),
            out.status,
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn at_pin(dir: &Path, commit: &str) -> bool {
    dir.join(".git").exists() && git(dir, &["rev-parse", "HEAD"]).ok().as_deref() == Some(commit)
}

/// Remove `<name>.part.<pid>.<tid>` directories left by killed runs.
/// Only entries whose pid is not a live process are removed, so a concurrent
/// `ensure` (another process, or another thread of this one) is never touched.
fn sweep_orphaned_parts(cache: &Path) {
    let Ok(rd) = std::fs::read_dir(cache) else { return };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some((_, rest)) = name.to_str().and_then(|n| n.split_once(".part.")) else { continue };
        let Some(pid) = rest.split('.').next().and_then(|p| p.parse::<u32>().ok()) else { continue };
        if e.file_type().is_ok_and(|t| t.is_dir()) && !pid_alive(pid) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// `ps -p` succeeds for any existing process regardless of owner; if `ps`
/// itself cannot run, assume alive so the sweep errs toward keeping data.
fn pid_alive(pid: u32) -> bool {
    Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true)
}

/// `dest` may be replaced only if absent or a git repository.
fn guard_dest(dest: &Path) -> Result<()> {
    match std::fs::symlink_metadata(dest) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::msg(format!("stat {}: {e}", dest.display()))),
        Ok(_) if dest.join(".git").exists() => Ok(()),
        Ok(_) => Err(Error::msg(format!(
            "{} exists but is not a projectmp4 preset cache (no .git entry); refusing to delete it",
            dest.display()
        ))),
    }
}

fn ensure_source(cache: &Path, s: &Source) -> Result<()> {
    let dest = cache.join(&s.name);
    if at_pin(&dest, &s.commit) {
        return Ok(());
    }
    guard_dest(&dest)?; // fail before spending a network fetch
    // Build in a sibling `.part` directory and rename, so an interrupted
    // fetch never leaves a half-populated pack at the final path.
    // Unique per process and thread so concurrent callers cannot trample each other.
    let part = cache.join(format!("{}.part.{}.{:?}", s.name, std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&part);
    std::fs::create_dir_all(&part)
        .map_err(|e| Error::msg(format!("create {}: {e}", part.display())))?;
    let built = (|| {
        git(&part, &["init", "--quiet"])?;
        git(&part, &["remote", "add", "origin", &s.repo])?;
        git(&part, &["fetch", "--quiet", "--depth", "1", "origin", &s.commit])?;
        git(&part, &["checkout", "--quiet", "--detach", &s.commit])?;
        let head = git(&part, &["rev-parse", "HEAD"])?;
        if head != s.commit {
            return Err(Error::msg(format!("{}: checked out {head}, expected {}", s.name, s.commit)));
        }
        Ok(())
    })();
    if let Err(e) = built {
        let _ = std::fs::remove_dir_all(&part);
        return Err(e);
    }
    if at_pin(&dest, &s.commit) {
        // A concurrent caller finished first; keep theirs.
        let _ = std::fs::remove_dir_all(&part);
        return Ok(());
    }
    if let Err(e) = guard_dest(&dest) {
        let _ = std::fs::remove_dir_all(&part);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&part, &dest)
        .map_err(|e| Error::msg(format!("rename {} -> {}: {e}", part.display(), dest.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_sources_parse_and_are_full_shas() {
        let s = sources().unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].name, "presets-cream-of-the-crop");
        assert_eq!(s[0].commit, "0180df21f5e0bd39b9060cc5de420ed2f1f9e509");
    }

    fn entry(name: &str) -> Source {
        Source { name: name.into(), repo: "/nonexistent".into(), commit: "0".repeat(40) }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("projectmp4-unit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_pack_entry_is_an_error() {
        let cache = tmp("missing-entry");
        // classic and textures present, cotc absent
        let entries = [entry(CLASSIC), entry(TEXTURES)];
        let err = Packs::ensure_from(&cache, &entries).unwrap_err().to_string();
        assert!(err.contains(COTC), "{err}");
        assert!(Packs::ensure_from(&cache, &[]).is_err());
        let _ = std::fs::remove_dir_all(&cache);
    }

    /// A one-commit local repository to fetch from, so the test needs no network.
    fn local_repo(dir: &Path) -> String {
        let run = |args: &[&str]| {
            let o = Command::new("git").arg("-C").arg(dir).args(args)
                .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
                .output().unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        std::fs::create_dir_all(dir).unwrap();
        run(&["init", "--quiet"]);
        std::fs::write(dir.join("a.milk"), "x").unwrap();
        run(&["add", "."]);
        run(&["commit", "--quiet", "-m", "c"]);
        run(&["rev-parse", "HEAD"])
    }

    #[test]
    fn non_git_dest_is_refused_and_its_data_survives() {
        let root = tmp("guard");
        let repo = root.join("origin");
        let commit = local_repo(&repo);
        let cache = root.join("cache");
        let s = Source { name: "pack".into(), repo: repo.display().to_string(), commit };
        let dest = cache.join("pack");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("precious.txt"), "mine").unwrap();

        let res = ensure_source(&cache, &s);
        assert!(dest.join("precious.txt").exists(), "user data was deleted");
        let err = res.unwrap_err().to_string();
        assert!(err.contains("not a projectmp4 preset cache"), "{err}");
        assert!(err.contains(&dest.display().to_string()), "{err}");

        // Control: once the dest is absent the same call succeeds, so the error above
        // came from the guard and not from a broken fixture.
        std::fs::remove_dir_all(&dest).unwrap();
        ensure_source(&cache, &s).unwrap();
        assert!(dest.join("a.milk").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn short_sha_is_rejected() {
        let bad = "[[source]]\nname=\"a\"\nrepo=\"b\"\ncommit=\"abc\"\n";
        assert!(parse_sources(bad).is_err());
    }
}
