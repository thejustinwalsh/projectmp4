//! The binding profile: raw MIDI bytes in, a verb out.
//!
//! A profile is a line-based text file, one binding per line:
//!
//! ```text
//! name = "Device"
//! <status>:<number> = <verb> [arguments]   # comment
//! ```
//!
//! Status and number are `0x`-prefixed hex. See `profiles/midi/` for the
//! shipped profiles and the verb table in [`parse_verb`].
//!
//! # Toggles
//!
//! A profile owns no preview state, so `lock toggle`, `cut-style toggle` and
//! `flash-guard toggle` cannot be decided here. They are resolved against the
//! live state the caller passes to [`Profile::resolve_with`], which returns the
//! verb carrying the flipped value. Plain [`Profile::resolve`] has no state and
//! resolves a toggle to the "turn on" value (`Lock(true)`, `FlashGuard(true)`,
//! `CutStyle { smooth: true }`): the preview must call `resolve_with`, never
//! `resolve`, or a toggle button will only ever switch things on.
//!
//! # `print-preset`
//!
//! The profile cannot know the current preset. It resolves to
//! `Action::Print(PathBuf::new())`; the caller substitutes the live path.

use crate::control::{Action, StemSource, STEM_BLEND_RANGE};
use crate::ffi::{Error, Result};
use std::path::{Path, PathBuf};

/// The live state toggles are flipped against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Current {
    pub locked: bool,
    pub smooth_cuts: bool,
    pub flash_guard: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Param {
    Drive,
    BeatSensitivity,
    StemBlend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    Lock,
    CutStyle,
    FlashGuard,
}

#[derive(Debug, Clone)]
enum Binding {
    /// Fires its action on a button press.
    Button(Action),
    Toggle(Toggle),
    /// Value 0..=127 scaled into `lo..=hi`.
    Abs { param: Param, lo: f32, hi: f32 },
    /// Relative encoder driving `step-preset`.
    Relative,
    /// High half of a 14-bit pair; stores into `pending[slot]`, fires nothing.
    Msb { slot: usize, param: Param, lo: f32, hi: f32 },
    /// Low half: combines with `pending[slot]` and fires.
    Lsb { slot: usize, param: Param, lo: f32, hi: f32 },
}

/// A parsed binding profile.
#[derive(Debug, Clone)]
pub struct Profile {
    name: String,
    /// Sorted by key `(status << 8) | number`, searched by binary search:
    /// no allocation and no hashing per message.
    bindings: Vec<(u16, Binding)>,
    /// Pending MSB per 14-bit pair, held between messages.
    pending: Vec<Option<u8>>,
}

fn key(status: u8, number: u8) -> u16 {
    ((status as u16) << 8) | number as u16
}

fn lerp(lo: f32, hi: f32, t: f32) -> f32 {
    // Written so t == 0 gives lo and t == 1 gives hi exactly.
    lo * (1.0 - t) + hi * t
}

fn make(param: Param, v: f32) -> Action {
    match param {
        Param::Drive => Action::drive(v),
        Param::BeatSensitivity => Action::beat_sensitivity(v),
        Param::StemBlend => Action::stem_blend(v),
    }
}

/// Removes a `#` comment, ignoring `#` inside a double-quoted string.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

fn hex(s: &str) -> Option<u8> {
    let s = s.trim();
    let digits = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    u8::from_str_radix(digits, 16).ok()
}

fn parse_address(lhs: &str, line: &str) -> Result<(u8, u8)> {
    let bad = || Error::msg(format!("malformed address {lhs:?} (want 0xSS:0xNN) in line {line:?}"));
    let (s, n) = lhs.split_once(':').ok_or_else(bad)?;
    let status = hex(s).ok_or_else(bad)?;
    let number = hex(n).ok_or_else(bad)?;
    if !(0x80..=0xEF).contains(&status) {
        return Err(Error::msg(format!(
            "status 0x{status:02x} in {lhs:?} is not a channel-voice status (0x80..=0xef) in line {line:?}"
        )));
    }
    if number > 0x7F {
        return Err(Error::msg(format!("number 0x{number:02x} in {lhs:?} exceeds 0x7f in line {line:?}")));
    }
    Ok((status, number))
}

fn float(s: &str, line: &str) -> Result<f32> {
    match s.parse::<f32>() {
        Ok(v) if v.is_finite() => Ok(v),
        _ => Err(Error::msg(format!("bad number {s:?} in line {line:?}"))),
    }
}

fn range(lo: &str, hi: &str, line: &str) -> Result<(f32, f32)> {
    let (lo, hi) = (float(lo, line)?, float(hi, line)?);
    if lo >= hi {
        return Err(Error::msg(format!("empty or inverted range {lo}..{hi} (lo must be below hi) in line {line:?}")));
    }
    Ok((lo, hi))
}

fn flag(args: &[&str], line: &str, words: &[&str]) -> Result<usize> {
    match args {
        [w] => words
            .iter()
            .position(|x| x == w)
            .ok_or_else(|| Error::msg(format!("argument {w:?} must be one of {} in line {line:?}", words.join(" | ")))),
        _ => Err(Error::msg(format!("expected one argument ({}) in line {line:?}", words.join(" | ")))),
    }
}

fn none(args: &[&str], line: &str) -> Result<()> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(Error::msg(format!("unexpected arguments in line {line:?}")))
    }
}

/// A binding as parsed, before 14-bit pairs are linked.
enum Raw {
    Done(Binding),
    Msb { param: Param, lo: f32, hi: f32 },
    Lsb { param: Param },
}

fn parse_verb(verb: &str, args: &[&str], line: &str) -> Result<Raw> {
    use Action as A;
    let button = |a: Action| Ok(Raw::Done(Binding::Button(a)));
    let abs = |param: Param| -> Result<Raw> {
        let [lo, hi] = args else {
            return Err(Error::msg(format!("{verb} takes `lo hi` in line {line:?}")));
        };
        let (lo, hi) = range(lo, hi, line)?;
        Ok(Raw::Done(Binding::Abs { param, lo, hi }))
    };
    let pair = |param: Param, ranged: bool| -> Result<Raw> {
        match (args, ranged) {
            (["lsb"], _) => Ok(Raw::Lsb { param }),
            (["msb", lo, hi], true) => {
                let (lo, hi) = range(lo, hi, line)?;
                Ok(Raw::Msb { param, lo, hi })
            }
            (["msb"], false) => Ok(Raw::Msb { param, lo: STEM_BLEND_RANGE.0, hi: STEM_BLEND_RANGE.1 }),
            _ => Err(Error::msg(format!(
                "{verb} takes {} in line {line:?}",
                if ranged { "`msb lo hi` or `lsb`" } else { "`msb` or `lsb`" }
            ))),
        }
    };
    match verb {
        "transport-toggle" => none(args, line).and_then(|_| button(A::TransportToggle)),
        "resync" => none(args, line).and_then(|_| button(A::Resync)),
        "quit" => none(args, line).and_then(|_| button(A::Quit)),
        "print-preset" => none(args, line).and_then(|_| button(A::Print(PathBuf::new()))),
        "print-state" => none(args, line).and_then(|_| button(A::PrintState)),
        "step-preset" => match flag(args, line, &["forward", "back", "relative"])? {
            0 => button(A::StepPreset { forward: true }),
            1 => button(A::StepPreset { forward: false }),
            _ => Ok(Raw::Done(Binding::Relative)),
        },
        "jump-preset" => {
            let [s] = args else {
                return Err(Error::msg(format!("jump-preset takes a slot in line {line:?}")));
            };
            let slot = s.parse::<u8>().map_err(|_| Error::msg(format!("bad slot {s:?} in line {line:?}")))?;
            button(A::JumpPreset { slot })
        }
        "seek-by" => {
            let [s] = args else {
                return Err(Error::msg(format!("seek-by takes seconds in line {line:?}")));
            };
            match s.parse::<f64>() {
                Ok(secs) if secs.is_finite() => button(A::SeekBy { secs }),
                _ => Err(Error::msg(format!("bad seconds {s:?} in line {line:?}"))),
            }
        }
        "cut-style" => match flag(args, line, &["toggle", "smooth", "hard"])? {
            0 => Ok(Raw::Done(Binding::Toggle(Toggle::CutStyle))),
            i => button(A::CutStyle { smooth: i == 1 }),
        },
        "lock" => match flag(args, line, &["toggle", "on", "off"])? {
            0 => Ok(Raw::Done(Binding::Toggle(Toggle::Lock))),
            i => button(A::Lock(i == 1)),
        },
        "flash-guard" => match flag(args, line, &["toggle", "on", "off"])? {
            0 => Ok(Raw::Done(Binding::Toggle(Toggle::FlashGuard))),
            i => button(A::FlashGuard(i == 1)),
        },
        "stem-snap" => match flag(args, line, &["all", "vocals", "instrumental"])? {
            0 => button(A::StemSnap(StemSource::All)),
            1 => button(A::StemSnap(StemSource::Vocals)),
            _ => button(A::StemSnap(StemSource::Instrumental)),
        },
        "stem-blend" => none(args, line).map(|_| {
            Raw::Done(Binding::Abs { param: Param::StemBlend, lo: STEM_BLEND_RANGE.0, hi: STEM_BLEND_RANGE.1 })
        }),
        "drive" => abs(Param::Drive),
        "beat-sensitivity" => abs(Param::BeatSensitivity),
        "drive-14" => pair(Param::Drive, true),
        "beat-sensitivity-14" => pair(Param::BeatSensitivity, true),
        "stem-blend-14" => pair(Param::StemBlend, false),
        other => Err(Error::msg(format!("unknown verb {other:?} in line {line:?}"))),
    }
}

impl Profile {
    pub fn parse(text: &str) -> Result<Profile> {
        let mut name: Option<String> = None;
        let mut raw: Vec<(u8, u8, Raw, String)> = Vec::new();

        for line in text.lines().map(strip_comment).map(str::trim).filter(|l| !l.is_empty()) {
            let (lhs, rhs) = line
                .split_once('=')
                .ok_or_else(|| Error::msg(format!("unrecognised line {line:?} (want `<status>:<number> = <verb>`)")))?;
            let (lhs, rhs) = (lhs.trim(), rhs.trim());
            if lhs == "name" {
                let n = rhs
                    .strip_prefix('"')
                    .and_then(|r| r.strip_suffix('"'))
                    .filter(|n| !n.contains('"'))
                    .ok_or_else(|| Error::msg(format!("name must be a quoted string in line {line:?}")))?;
                name = Some(n.to_string());
                continue;
            }
            let (status, number) = parse_address(lhs, line)?;
            if raw.iter().any(|(s, n, _, _)| (*s, *n) == (status, number)) {
                return Err(Error::msg(format!("duplicate address 0x{status:02x}:0x{number:02x} in line {line:?}")));
            }
            let mut words = rhs.split_whitespace();
            let verb = words.next().ok_or_else(|| Error::msg(format!("no verb in line {line:?}")))?;
            let args: Vec<&str> = words.collect();
            raw.push((status, number, parse_verb(verb, &args, line)?, line.to_string()));
        }

        let name = name
            .filter(|n| !n.is_empty())
            .ok_or_else(|| Error::msg("profile has no `name = \"...\"` line"))?;

        // Link each 14-bit pair: the LSB sits at the MSB's number + 0x20.
        let mut bindings: Vec<(u16, Binding)> = Vec::with_capacity(raw.len());
        let mut pending = Vec::new();
        for (status, number, r, line) in &raw {
            let find = |n: u8| raw.iter().find(|(s, m, _, _)| (*s, *m) == (*status, n)).map(|e| &e.2);
            let b = match r {
                Raw::Done(b) => b.clone(),
                Raw::Msb { param, lo, hi } => {
                    let lsb = number.checked_add(0x20).filter(|n| *n <= 0x7F).and_then(find);
                    match lsb {
                        Some(Raw::Lsb { param: p }) if p == param => {}
                        _ => {
                            return Err(Error::msg(format!(
                                "msb at 0x{status:02x}:0x{number:02x} has no matching lsb at 0x{status:02x}:0x{:02x} (same verb) in line {line:?}",
                                number.wrapping_add(0x20)
                            )))
                        }
                    }
                    pending.push(None);
                    Binding::Msb { slot: pending.len() - 1, param: *param, lo: *lo, hi: *hi }
                }
                Raw::Lsb { .. } => continue, // linked below, once its MSB has a slot
            };
            bindings.push((key(*status, *number), b));
        }
        for (status, number, r, line) in &raw {
            let Raw::Lsb { param } = r else { continue };
            let msb = number
                .checked_sub(0x20)
                .and_then(|n| bindings.iter().find(|(k, _)| *k == key(*status, n)));
            match msb {
                Some((_, Binding::Msb { slot, param: p, lo, hi })) if p == param => {
                    bindings.push((key(*status, *number), Binding::Lsb { slot: *slot, param: *param, lo: *lo, hi: *hi }));
                }
                _ => {
                    return Err(Error::msg(format!(
                        "lsb at 0x{status:02x}:0x{number:02x} has no matching msb at 0x{status:02x}:0x{:02x} (same verb) in line {line:?}",
                        number.wrapping_sub(0x20)
                    )))
                }
            }
        }
        bindings.sort_by_key(|(k, _)| *k);
        Ok(Profile { name, bindings, pending })
    }

    pub fn load(path: &Path) -> Result<Profile> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::msg(format!("cannot read MIDI profile {}: {e}", path.display())))?;
        Profile::parse(&text).map_err(|e| Error::msg(format!("{}: {}", path.display(), e.0)))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn binding_count(&self) -> usize {
        self.bindings.len()
    }

    /// Stateless resolve: toggles come back as the "turn on" value. The
    /// preview should call [`Profile::resolve_with`] so toggles flip.
    pub fn resolve(&mut self, msg: [u8; 3]) -> Option<Action> {
        self.lookup(msg, None)
    }

    /// Resolve with the live state, so `toggle` bindings return the flipped value.
    pub fn resolve_with(&mut self, msg: [u8; 3], current: &Current) -> Option<Action> {
        self.lookup(msg, Some(current))
    }

    /// Allocation-free: binary search, then plain arithmetic.
    fn lookup(&mut self, [status, number, value]: [u8; 3], cur: Option<&Current>) -> Option<Action> {
        let i = self.bindings.binary_search_by_key(&key(status, number), |(k, _)| *k).ok()?;
        // Velocity 0 is a note-off by convention; 0x8_ is one outright.
        let pressed = status & 0xF0 != 0x80 && value > 0;
        match &self.bindings[i].1 {
            Binding::Button(a) => pressed.then(|| a.clone()),
            Binding::Toggle(t) => {
                if !pressed {
                    return None;
                }
                Some(match t {
                    Toggle::Lock => Action::Lock(cur.is_none_or(|c| !c.locked)),
                    Toggle::CutStyle => Action::CutStyle { smooth: cur.is_none_or(|c| !c.smooth_cuts) },
                    Toggle::FlashGuard => Action::FlashGuard(cur.is_none_or(|c| !c.flash_guard)),
                })
            }
            Binding::Abs { param, lo, hi } => {
                if status & 0xF0 == 0x80 {
                    return None;
                }
                Some(make(*param, lerp(*lo, *hi, value.min(127) as f32 / 127.0)))
            }
            // This device family disagrees with itself about whether 0x40 means
            // +64 or -64 (two's complement vs. sign-magnitude firmware), so it
            // is treated as no movement rather than guessed. 0x01..=0x3F is
            // forward and 0x41..=0x7F backward, so a magnitude still works.
            Binding::Relative => match value {
                0x01..=0x3F => Some(Action::StepPreset { forward: true }),
                0x41..=0x7F => Some(Action::StepPreset { forward: false }),
                _ => None,
            },
            Binding::Msb { slot, .. } => {
                self.pending[*slot] = Some(value & 0x7F);
                None
            }
            Binding::Lsb { slot, param, lo, hi } => {
                let msb = self.pending[*slot]?;
                let v = ((msb as u16) << 7) | (value & 0x7F) as u16;
                Some(make(*param, lerp(*lo, *hi, v as f32 / 16383.0)))
            }
        }
    }
}
