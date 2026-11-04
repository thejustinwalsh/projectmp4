//! Fetch the pinned MilkDrop preset packs into a cache directory.
//!
//! The pack contents are third-party data: they are cloned, never executed.

use crate::ffi::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

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
        std::fs::create_dir_all(cache)
            .map_err(|e| Error::msg(format!("create {}: {e}", cache.display())))?;
        for s in sources()? {
            ensure_source(cache, &s)?;
        }
        Ok(Packs {
            cotc: cache.join("presets-cream-of-the-crop"),
            classic: cache.join("presets-projectm-classic"),
            textures: cache.join("presets-milkdrop-texture-pack").join("textures"),
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
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
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

fn ensure_source(cache: &Path, s: &Source) -> Result<()> {
    let dest = cache.join(&s.name);
    if at_pin(&dest, &s.commit) {
        return Ok(());
    }
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

    #[test]
    fn short_sha_is_rejected() {
        let bad = "[[source]]\nname=\"a\"\nrepo=\"b\"\ncommit=\"abc\"\n";
        assert!(parse_sources(bad).is_err());
    }
}
