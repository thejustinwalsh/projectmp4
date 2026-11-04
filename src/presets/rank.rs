//! Pick presets per genre: human curation tiers crossed with bass reactivity
//! measured from each preset's own equation text.
//!
//! `score_milk` is a line-for-line port of the `score()` function in the analysis
//! prototype (`score_cotc.py`), including its quirks, except the de-nesting
//! subtraction (a prototype bug, removed). The committed fixture
//! `tests/fixtures/cotc_scores.json` is the prototype's output regenerated with that fix.
//! Preset files are untrusted data: they are only ever read as text, never run.

use crate::ffi::{Error, Result};
use crate::presets::manifest::unquote;
use crate::presets::Packs;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Score {
    /// `3 * (bass + bass_att)`.
    pub bass: u32,
    /// Weighted sum of all eight audio variables; includes the `bass` term.
    pub react: u32,
    pub shader: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub presets: Vec<PathBuf>,
}

/// Every profile `Manifest::generate` writes, in file order.
pub const PROFILES: [&str; 4] = ["deathstep", "dubstep", "dancehall", "ambient"];

/// Files larger than this are skipped as ineligible rather than read into memory.
const MAX_MILK_BYTES: u64 = 4 * 1024 * 1024;

/// (variable, weight). Order is irrelevant to the result.
const VARS: [(&str, i64); 8] = [
    ("bass_att", 3),
    ("bass", 3),
    ("treb_att", 1),
    ("treb", 1),
    ("mid_att", 1),
    ("mid", 1),
    ("vol_att", 1),
    ("vol", 1),
];

const EQ_PREFIXES: [&str; 8] =
    ["per_frame", "per_pixel", "per_point", "wave_", "shapecode", "wavecode", "warp_", "comp_"];

/// Python `re` `\s` on `str`: ASCII whitespace plus `\x1c`-`\x1f` and Unicode whitespace.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python `re` `\w` on `str`.
fn py_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Python `str.splitlines()` (without line ends).
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            out.push(&s[start..i]);
            start = i + c.len_utf8();
            if c == '\r' && it.peek().is_some_and(|&(_, n)| n == '\n') {
                it.next();
                start += 1;
            }
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn skip_space(s: &str, i: usize) -> usize {
    i + s[i..].chars().take_while(|&c| py_space(c)).map(char::len_utf8).sum::<usize>()
}

/// Right-hand side of `l` if it is an equation line:
/// `^\s*(per_frame|...|comp_)\w*\s*=` in the prototype.
fn equation_rhs(l: &str) -> Option<&str> {
    let t = &l[skip_space(l, 0)..];
    let rest = EQ_PREFIXES.iter().find_map(|p| t.strip_prefix(p))?;
    let rest = rest.trim_start_matches(py_word);
    let rest = &rest[skip_space(rest, 0)..];
    rest.strip_prefix('=')
}

/// `(?<![a-z_])k(?![a-z_0-9])` match count.
fn count_word(code: &str, k: &str) -> usize {
    code.match_indices(k)
        .filter(|&(i, _)| {
            let before = code[..i].chars().next_back();
            let after = code[i + k.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                && !after.is_some_and(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
        })
        .count()
}

/// `^\s*(warp|comp)_1\s*=\s*\S` with `re.M` over the whole text. `\s*` crosses
/// newlines, so `warp_1=` followed by a blank line and then text still matches.
fn has_shader_line(low: &str) -> bool {
    let mut i = 0;
    loop {
        let j = skip_space(low, i);
        let t = &low[j..];
        if let Some(r) = t.strip_prefix("warp_1").or_else(|| t.strip_prefix("comp_1")) {
            let r = &r[skip_space(r, 0)..];
            if let Some(r) = r.strip_prefix('=') {
                if !r[skip_space(r, 0)..].is_empty() {
                    return true;
                }
            }
        }
        // Every line start inside the whitespace run [i, j) reaches the same j and fails
        // the same way, so resume at the first line start at or after j (keeps this linear).
        match low[j..].find('\n') {
            Some(n) => i = j + n + 1,
            None => return false,
        }
    }
}

pub fn score_milk(text: &str) -> Score {
    let low = text.to_lowercase();
    let code = py_splitlines(&low).into_iter().filter_map(equation_rhs).collect::<Vec<_>>().join(" ");
    let mut c = [0i64; 8];
    for (n, (k, _)) in VARS.iter().enumerate() {
        c[n] = count_word(&code, k) as i64;
    }
    // No de-nesting subtraction (bare -= _att): `count_word` already rejects `bass_att` when
    // matching `bass`, so the counts are exact. The prototype subtracted anyway, which made
    // each pair contribute max(raw, att) instead of raw + att. That was a bug, fixed here.
    let react: i64 = c.iter().zip(VARS).map(|(n, (_, w))| n * w).sum();
    let bass = 3 * (c[0] + c[1]);
    Score {
        bass: u32::try_from(bass).unwrap_or(u32::MAX),
        react: u32::try_from(react).unwrap_or(u32::MAX),
        shader: has_shader_line(&low) || low.contains("shader_body"),
    }
}

/// Score raw file bytes the way the prototype read them: as Latin-1, which never fails.
pub fn score_bytes(b: &[u8]) -> Score {
    score_milk(&b.iter().map(|&x| x as char).collect::<String>())
}

/// Name key shared by the blacklist and the tier lookup: file name without `.milk`,
/// lowercased, runs of non `[a-z0-9]` collapsed to one space.
pub(crate) fn norm(name: &str) -> String {
    let lower = name.to_lowercase();
    let stem = lower.strip_suffix(".milk").unwrap_or(&lower);
    let mut out = String::new();
    let mut gap = false;
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(ch);
        } else {
            gap = true;
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlacklistEntry {
    pub key: String,
    pub prefix: bool,
    pub reason: String,
}

const BLACKLIST_TOML: &str = include_str!("../../presets/blacklist.toml");

pub(crate) fn parse_blacklist(text: &str) -> Result<Vec<BlacklistEntry>> {
    let mut out: Vec<(Option<String>, bool, Option<String>)> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if line == "[[preset]]" {
            out.push((None, false, None));
            continue;
        }
        let (k, v) = line.split_once('=').ok_or_else(|| Error::msg(format!("blacklist.toml: bad line {line:?}")))?;
        let (v, rest) = unquote(v.trim())?;
        if !rest.trim().is_empty() {
            return Err(Error::msg(format!("blacklist.toml: trailing text in {line:?}")));
        }
        let cur = out.last_mut().ok_or_else(|| Error::msg("blacklist.toml: key outside [[preset]]"))?;
        match k.trim() {
            "name" => cur.0 = Some(norm(&v)),
            "reason" => cur.2 = Some(v),
            "match" => {
                cur.1 = match v.as_str() {
                    "exact" => false,
                    "prefix" => true,
                    other => return Err(Error::msg(format!("blacklist.toml: unknown match {other:?}"))),
                }
            }
            other => return Err(Error::msg(format!("blacklist.toml: unknown key {other:?}"))),
        }
    }
    out.into_iter()
        .map(|(n, prefix, r)| match (n, r) {
            (Some(key), Some(reason)) if !key.is_empty() => Ok(BlacklistEntry { key, prefix, reason }),
            _ => Err(Error::msg("blacklist.toml: [[preset]] needs a name and a reason")),
        })
        .collect()
}

pub(crate) fn blacklist() -> Vec<BlacklistEntry> {
    parse_blacklist(BLACKLIST_TOML).expect("embedded presets/blacklist.toml is valid")
}

pub(crate) fn is_blacklisted(list: &[BlacklistEntry], file_name: &str) -> bool {
    let n = norm(file_name);
    list.iter().any(|e| if e.prefix { n.starts_with(&e.key) } else { n == e.key })
}

/// Genre weighting. `score = bass_w * bass + other_w * (react - bass) + tiers`.
struct Genre {
    cats: &'static [&'static str],
    bass_w: i64,
    other_w: i64,
}

fn genre(profile: &str) -> Option<Genre> {
    Some(match profile {
        "deathstep" | "dubstep" => Genre { cats: &["Reaction", "Geometric", "Supernova"], bass_w: 4, other_w: 1 },
        "dancehall" => Genre { cats: &["Reaction", "Dancer"], bass_w: 2, other_w: 2 },
        // Calm and shimmering: penalise kick-following, reward mid/treble/volume drift.
        "ambient" => Genre { cats: &["Hypnotic", "Fractal"], bass_w: -2, other_w: 1 },
        _ => return None,
    })
}

/// Bonus for also appearing in the BLTC best-of (a second human curation pass).
const TIER_BLTC: i64 = 100;
/// Bonus for also appearing in the Winamp stock set.
const TIER_STOCK: i64 = 40;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub score: Score,
    /// Genre-weighted bass score plus tier bonuses; what `rank` sorts by.
    pub rank_score: i64,
}

fn walk_milk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        // file_type() does not follow symlinks: a link in an untrusted pack is ignored.
        let Ok(t) = e.file_type() else { continue };
        let p = e.path();
        if t.is_dir() {
            walk_milk(&p, out);
        } else if t.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("milk")) {
            out.push(p);
        }
    }
}

fn name_set(dir: &Path) -> HashSet<String> {
    let mut v = Vec::new();
    walk_milk(dir, &mut v);
    v.iter().filter_map(|p| p.file_name()?.to_str().map(norm)).collect()
}

/// Read and score a file; `None` if unreadable or larger than `MAX_MILK_BYTES`.
fn score_file(p: &Path) -> Option<Score> {
    if std::fs::metadata(p).ok()?.len() > MAX_MILK_BYTES {
        return None;
    }
    Some(score_bytes(&std::fs::read(p).ok()?))
}

/// Every shader preset in the profile's categories that is not blacklisted,
/// best first (ties broken by path so the order is deterministic).
pub fn eligible(packs: &Packs, profile: &str) -> Result<Vec<Candidate>> {
    let g = genre(profile).ok_or_else(|| {
        Error::msg(format!("unknown profile {profile:?}; expected one of {}", PROFILES.join(", ")))
    })?;
    let bl = blacklist();
    let bltc = name_set(&packs.bltc());
    let stock = name_set(&packs.stock());
    let mut files = Vec::new();
    for cat in g.cats {
        walk_milk(&packs.cotc.join(cat), &mut files);
    }
    let mut out: Vec<Candidate> = files
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            if is_blacklisted(&bl, name) {
                return None;
            }
            let score = score_file(&path)?;
            if !score.shader {
                return None;
            }
            let key = norm(name);
            let mut rank_score = g.bass_w * i64::from(score.bass) + g.other_w * (i64::from(score.react) - i64::from(score.bass));
            if bltc.contains(&key) {
                rank_score += TIER_BLTC;
            }
            if stock.contains(&key) {
                rank_score += TIER_STOCK;
            }
            Some(Candidate { path, score, rank_score })
        })
        .collect();
    out.sort_by(|a, b| b.rank_score.cmp(&a.rank_score).then_with(|| a.path.cmp(&b.path)));
    Ok(out)
}

pub fn rank(packs: &Packs, profile: &str, n: usize) -> Result<Profile> {
    let presets = eligible(packs, profile)?.into_iter().take(n).map(|c| c.path).collect();
    Ok(Profile { name: profile.to_string(), presets })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bass_att_is_not_double_counted_as_bass() {
        assert_eq!(score_milk("per_frame_1=zoom=1+bass_att*0.1;").bass, 3);
        assert_eq!(score_milk("per_frame_1=zoom=1+bass*0.1;").bass, 3);
        // No de-nesting subtraction, on purpose: the lookahead already rejects `_`, so the
        // bare `bass` count excludes `bass_att` and the two simply add. The prototype
        // subtracted `_att` from the bare count anyway (scoring max, not sum); do not restore it.
        assert_eq!(score_milk("per_frame_1=a=bass+bass_att;").bass, 6);
        assert_eq!(score_milk("per_frame_1=a=bass+bass+bass_att;").bass, 9);
    }

    #[test]
    fn only_equation_lines_count() {
        assert_eq!(score_milk("fRating=5.0\nsTitle=bass bass bass\n").bass, 0);
    }

    #[test]
    fn word_boundaries_follow_the_prototype() {
        // Digit before the word is allowed by the prototype; digit/underscore/letter after is not.
        assert_eq!(score_milk("per_frame_1=a=2bass;").bass, 3);
        assert_eq!(score_milk("per_frame_1=a=bass2+bass_x+bassy+xbass+_bass;").bass, 0);
        assert_eq!(score_milk("per_frame_1=a=Bass+MID;").react, 4);
    }

    #[test]
    fn react_includes_the_bass_term() {
        let s = score_milk("per_frame_1=a=bass_att+treb+mid_att+vol;");
        assert_eq!((s.bass, s.react), (3, 6));
    }

    #[test]
    fn shader_detection() {
        assert!(score_milk("warp_1=`shader_type(ps_2_0)").shader);
        assert!(score_milk("  comp_1 = x").shader);
        assert!(!score_milk("warp_1=\n").shader);
        assert!(!score_milk("warp_1=   \n  \n").shader);
        assert!(!score_milk("sample_warp_1=x\nwarp_2=x").shader);
        assert!(score_milk("junk\nshader_body\n").shader);
        // `\s*` crosses newlines in the prototype: an empty first line followed by text counts.
        assert!(score_milk("warp_1=\nfoo").shader);
    }

    #[test]
    fn hostile_input_does_not_panic() {
        let _ = score_milk("");
        let _ = score_milk("=");
        let _ = score_milk("\r\r\r\n\x0b\x0c\u{85}per_frame=\u{2028}");
        let _ = score_milk(&"\n".repeat(200_000));
        let _ = score_milk(&"warp_1\n".repeat(50_000));
        let _ = score_bytes(&[0xff, 0xfe, 0x00, 0x80]);
        let _ = score_milk("per_frame_1=ǅİß\u{1F4A9}bass");
    }

    #[test]
    fn blacklist_matches_the_documented_presets_only() {
        let bl = blacklist();
        assert_eq!(bl.len(), 5);
        for hit in [
            "martin - mandelbox explorer v1 nz+3.milk",
            "suksma - crotch connected siamese.milk",
            "$$$ Royal - Mashup (452).milk",
            "Stahlregen & EoS + Phat + Pithlit + Unchained - Raw Mash Up.milk",
            "phliping out ovre normal duqs ranglre genes.milk",
        ] {
            assert!(is_blacklisted(&bl, hit), "{hit}");
        }
        for miss in [
            "martin - mandelbox explorer v1 nz+6.milk",
            "suksma - crotch connected siamese nz.milk",
            "Royal - Mashup (453).milk",
            "other - phliping.milk",
        ] {
            assert!(!is_blacklisted(&bl, miss), "{miss}");
        }
    }

    #[test]
    fn unknown_profile_is_an_error() {
        let packs = Packs { cotc: "/nonexistent".into(), classic: "/nonexistent".into(), textures: "/nonexistent".into() };
        assert!(rank(&packs, "polka", 5).is_err());
    }
}
