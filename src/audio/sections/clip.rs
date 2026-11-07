//! Choosing the span `--clip` renders: the best chorus (the "drop") or the
//! intro, snapped to bars so the cut lands on the music's own grid.
//!
//! [`choose`] is pure; [`locate`] is the pipeline that feeds it (stems, the
//! section model, per-section energy).

use super::{postprocess, spectrogram, Section, Weights, LABELS};
use crate::audio::stems;
use crate::ffi::{Error, Result};
use std::path::Path;

const INTRO: u32 = 2;
const OUTRO: u32 = 3;
const CHORUS: u32 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipKind {
    #[default]
    Full,
    Drop,
    Intro,
}

impl ClipKind {
    pub fn name(self) -> &'static str {
        match self {
            ClipKind::Full => "full",
            ClipKind::Drop => "drop",
            ClipKind::Intro => "intro",
        }
    }

    pub fn parse(s: &str) -> Result<ClipKind> {
        match s {
            "full" => Ok(ClipKind::Full),
            "drop" => Ok(ClipKind::Drop),
            "intro" => Ok(ClipKind::Intro),
            _ => Err(Error::msg(format!("unknown clip {s:?}; expected full, drop or intro"))),
        }
    }
}

/// How the section the span came from was picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Highest-confidence `chorus`.
    Chorus,
    /// No `chorus`: the highest-energy section that is neither `intro` nor `outro`.
    LoudestSection,
    /// The `intro` section.
    Intro,
    /// `--clip intro` with no `intro` label: the first section.
    FirstSection,
    /// Nothing to pick from, or `want` covers the track: the whole track.
    WholeTrack,
}

impl Reason {
    pub fn name(self) -> &'static str {
        match self {
            Reason::Chorus => "chorus",
            Reason::LoudestSection => "loudest-section",
            Reason::Intro => "intro",
            Reason::FirstSection => "first-section",
            Reason::WholeTrack => "whole-track",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chosen {
    pub start: f64,
    pub end: f64,
    /// Index into `LABELS` of the section the span came from; `None` for `WholeTrack`.
    pub label: Option<u32>,
    pub reason: Reason,
}

fn whole(duration: f64) -> Chosen {
    Chosen { start: 0.0, end: duration, label: None, reason: Reason::WholeTrack }
}

/// RMS of `mono` over each section's span. One entry per section, same order.
/// `sr` must be non-zero. A section whose span lies entirely past the end of
/// `mono` scores 0.0.
pub fn section_energy(sections: &[Section], mono: &[f32], sr: u32) -> Result<Vec<f32>> {
    if sr == 0 {
        return Err(Error::msg("section_energy: sample rate is 0"));
    }
    sections
        .iter()
        .map(|s| {
            // `as usize` saturates and maps NaN to 0, so a malformed span is
            // empty here rather than a panic; `choose` is what rejects it.
            let at = |t: f64| ((t.max(0.0) * sr as f64).round() as usize).min(mono.len());
            let (a, b) = (at(s.start), at(s.end));
            if b <= a {
                return Ok(0.0);
            }
            let span = &mono[a..b];
            let sum: f64 = span.iter().map(|v| (*v as f64) * (*v as f64)).sum();
            Ok((sum / span.len() as f64).sqrt() as f32)
        })
        .collect()
}

/// `want` is the span length asked for; `None` means the chosen section's own
/// length. `duration` is the track's length in seconds. `downbeats` must be
/// sorted ascending and may be empty.
///
/// The span starts on the first downbeat at or after the section's start and
/// ends on the last downbeat at or before `start + want`, so it never exceeds
/// `want`: the `--target x` duration cap holds whatever tempo was detected.
pub fn choose(
    sections: &[Section],
    downbeats: &[f64],
    energy: &[f32],
    want: Option<f64>,
    duration: f64,
    kind: ClipKind,
) -> Result<Chosen> {
    if energy.len() != sections.len() {
        return Err(Error::msg(format!(
            "choose: {} energies for {} sections; they must pair up",
            energy.len(),
            sections.len()
        )));
    }
    if let Some(w) = want {
        if !w.is_finite() || w <= 0.0 {
            return Err(Error::msg(format!("choose: clip length must be a positive, finite number of seconds, got {w}")));
        }
    }
    if !duration.is_finite() || duration <= 0.0 {
        return Err(Error::msg(format!("choose: track duration must be a positive, finite number of seconds, got {duration}")));
    }
    for s in sections {
        if !s.start.is_finite() || !s.end.is_finite() || s.end <= s.start {
            return Err(Error::msg(format!("choose: section {}..{} is not a forward span", s.start, s.end)));
        }
    }
    if kind == ClipKind::Full || sections.is_empty() {
        return Ok(whole(duration));
    }

    // First maximum wins, so ties go to the earliest.
    let best = |idx: &mut dyn Iterator<Item = usize>, key: &dyn Fn(usize) -> f32| -> Option<usize> {
        idx.fold(None, |m, i| match m {
            Some(j) if key(i) <= key(j) => Some(j),
            _ => Some(i),
        })
    };
    let (pick, reason) = match kind {
        ClipKind::Drop => {
            if let Some(i) = best(&mut (0..sections.len()).filter(|&i| sections[i].label == CHORUS), &|i| sections[i].confidence) {
                (i, Reason::Chorus)
            } else {
                let by_energy = |i: usize| energy[i];
                let body = best(&mut (0..sections.len()).filter(|&i| !matches!(sections[i].label, INTRO | OUTRO)), &by_energy);
                let i = body.or_else(|| best(&mut (0..sections.len()), &by_energy)).expect("sections is non-empty");
                (i, Reason::LoudestSection)
            }
        }
        ClipKind::Intro => match sections.iter().position(|s| s.label == INTRO) {
            Some(i) => (i, Reason::Intro),
            None => (0, Reason::FirstSection),
        },
        ClipKind::Full => unreachable!("handled above"),
    };
    let cand = &sections[pick];
    let want = want.unwrap_or(cand.end - cand.start);
    if want >= duration {
        return Ok(whole(duration));
    }
    let start = downbeats
        .iter()
        .copied()
        .find(|&d| d >= cand.start)
        .filter(|&d| d < duration)
        .unwrap_or(cand.start);
    let limit = start + want;
    let end = downbeats
        .iter()
        .copied()
        .rfind(|&d| d > start && d <= limit)
        .unwrap_or(limit)
        .min(duration);
    if end <= start {
        return Ok(whole(duration));
    }
    Ok(Chosen { start, end, label: Some(cand.label), reason })
}

/// The section model's weights, shipped with the repository.
pub const SECTION_MODEL: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/models/harmonix-fold0-f32.gguf");

/// Stereo interleaved to mono, `(l + r) * 0.5`: the convention the measured
/// pipeline used.
#[allow(clippy::chunks_exact_to_as_chunks)] // the odd tail sample of a malformed buffer is dropped on purpose
fn mono(stereo: &[f32]) -> Vec<f32> {
    stereo.chunks_exact(2).map(|f| (f[0] + f[1]) * 0.5).collect()
}

/// Separate stems, label the sections, and choose the span. `wav` is a
/// normalised pipeline WAV (48 kHz stereo f32). `scratch` is a directory the
/// caller owns and will remove.
pub fn locate(
    wav: &Path,
    kind: ClipKind,
    want: Option<f64>,
    downbeats: &[f64],
    duration: f64,
    scratch: &Path,
    progress: bool,
) -> Result<Chosen> {
    if progress {
        eprintln!(
            "clip: separating stems to find the {} ({duration:.0} s of audio; expect roughly 3x that in wall clock)",
            kind.name()
        );
    }
    let rt = stems::Runtime::locate()?;
    let st = stems::separate(&rt, wav, scratch)?;
    let monos = [mono(&st.bass), mono(&st.drums), mono(&st.other), mono(&st.vocals)];
    drop(st);
    // The energy mix is the sum of the four stems, at 48 kHz.
    let mut mix = vec![0f32; monos[0].len()];
    for m in &monos {
        for (o, v) in mix.iter_mut().zip(m) {
            *o += v;
        }
    }
    let resampled = [
        spectrogram::resample_48k_to_44k1(&monos[0])?,
        spectrogram::resample_48k_to_44k1(&monos[1])?,
        spectrogram::resample_48k_to_44k1(&monos[2])?,
        spectrogram::resample_48k_to_44k1(&monos[3])?,
    ];
    drop(monos);
    let (spec, frames) = spectrogram::stack(&resampled)?;
    drop(resampled);
    let weights = Weights::load(Path::new(SECTION_MODEL))?;
    let logits = super::predict(&weights, &spec, frames)?;
    drop(spec);
    let sections = postprocess(&logits, frames)?;
    let energy = section_energy(&sections, &mix, 48_000)?;
    let chosen = choose(&sections, downbeats, &energy, want, duration, kind)?;
    if progress {
        let label = chosen.label.map_or("-", |l| LABELS[l as usize]);
        eprintln!(
            "clip: {:.1}..{:.1} s ({:.1} s), section {label}, picked as {}",
            chosen.start,
            chosen.end,
            chosen.end - chosen.start,
            chosen.reason.name()
        );
    }
    Ok(chosen)
}
