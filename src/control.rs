//! The verbs that drive the preview, independent of which device spoke them.
//!
//! A key press and a controller message both become an [`Action`], so there is
//! one code path from input to screen and one place that decides what each
//! verb means.

use crate::preview::Load;
use std::path::PathBuf;

/// Beat-detection sensitivity projectM starts with (vendored `setup.cpp` reads
/// its config key with this default). The neutral point for the control.
pub const DEFAULT_BEAT_SENSITIVITY: f32 = 1.0;

/// Which stems a snap to a pole selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StemSource {
    All,
    Vocals,
    Instrumental,
}

/// What an input asks the preview to do. Continuous values are built with the
/// clamping constructors; a variant filled in by hand skips the clamp, which
/// `RuntimeParams::apply` guards against again.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Quit,
    Load(Load),
    /// Print the current preset's path (so it can be pinned with `--preset`).
    Print(PathBuf),
    /// Lock changed; the value is the new state.
    Lock(bool),
    Ignored,
    StepPreset { forward: bool },
    JumpPreset { slot: u8 },
    Resync,
    TransportToggle,
    SeekBy { secs: f64 },
    CutStyle { smooth: bool },
    StemBlend(f32),
    StemSnap(StemSource),
    Drive(f32),
    BeatSensitivity(f32),
    FlashGuard(bool),
    PrintState,
}

/// Clamps into `lo..=hi`. NaN becomes `neutral`: `f32::clamp` would pass it
/// through, and a NaN gain silences or corrupts every frame after it.
fn bounded(v: f32, lo: f32, hi: f32, neutral: f32) -> f32 {
    if v.is_nan() {
        neutral
    } else {
        v.clamp(lo, hi)
    }
}

pub const DRIVE_RANGE: (f32, f32) = (0.0, 2.0);
pub const BEAT_SENSITIVITY_RANGE: (f32, f32) = (0.0, 5.0);
pub const STEM_BLEND_RANGE: (f32, f32) = (0.0, 1.0);

/// Neutral stem blend: an equal mix of the four stems reconstructs the
/// original track, so the midpoint is "unprocessed".
pub const NEUTRAL_STEM_BLEND: f32 = 0.5;
pub const NEUTRAL_DRIVE: f32 = 1.0;

impl Action {
    pub fn drive(v: f32) -> Action {
        Action::Drive(bounded(v, DRIVE_RANGE.0, DRIVE_RANGE.1, NEUTRAL_DRIVE))
    }

    pub fn beat_sensitivity(v: f32) -> Action {
        Action::BeatSensitivity(bounded(v, BEAT_SENSITIVITY_RANGE.0, BEAT_SENSITIVITY_RANGE.1, DEFAULT_BEAT_SENSITIVITY))
    }

    pub fn stem_blend(v: f32) -> Action {
        Action::StemBlend(bounded(v, STEM_BLEND_RANGE.0, STEM_BLEND_RANGE.1, NEUTRAL_STEM_BLEND))
    }
}

/// Whether a verb changed what is on screen, so a caller can tell a real
/// change from a repeat (a controller resends its knob position constantly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Changed,
    NoChange,
}

impl Applied {
    fn from_change(changed: bool) -> Applied {
        if changed {
            Applied::Changed
        } else {
            Applied::NoChange
        }
    }
}

/// The live state the continuous verbs move.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeParams {
    /// PCM gain into projectM.
    pub drive: f32,
    pub beat_sensitivity: f32,
    /// 0.0 bass+drums .. 1.0 other+vocals; 0.5 is the original mix.
    pub stem_blend: f32,
    pub stem_snap: StemSource,
    pub paused: bool,
    pub smooth_cuts: bool,
    pub flash_guard: bool,
}

impl RuntimeParams {
    pub fn new(flash_guard: bool, smooth_cuts: bool) -> RuntimeParams {
        RuntimeParams {
            drive: NEUTRAL_DRIVE,
            beat_sensitivity: DEFAULT_BEAT_SENSITIVITY,
            stem_blend: NEUTRAL_STEM_BLEND,
            stem_snap: StemSource::All,
            paused: false,
            smooth_cuts,
            flash_guard,
        }
    }

    /// `None` when the verb is not ours: the preview handles it against state
    /// only it holds (the schedule, the window, the audio clock).
    /// `Some(Changed | NoChange)` when applied, by whether any field moved.
    pub fn apply(&mut self, a: &Action) -> Option<Applied> {
        // Re-clamp through the constructors: a hand-built variant must not get
        // an out-of-range value past the type's guarantee.
        let changed = match *a {
            Action::Drive(v) => set(&mut self.drive, bounded(v, DRIVE_RANGE.0, DRIVE_RANGE.1, NEUTRAL_DRIVE)),
            Action::BeatSensitivity(v) => set(
                &mut self.beat_sensitivity,
                bounded(v, BEAT_SENSITIVITY_RANGE.0, BEAT_SENSITIVITY_RANGE.1, DEFAULT_BEAT_SENSITIVITY),
            ),
            Action::StemBlend(v) => set(&mut self.stem_blend, bounded(v, STEM_BLEND_RANGE.0, STEM_BLEND_RANGE.1, NEUTRAL_STEM_BLEND)),
            Action::StemSnap(s) => set(&mut self.stem_snap, s),
            Action::TransportToggle => {
                self.paused = !self.paused;
                true
            }
            Action::CutStyle { smooth } => set(&mut self.smooth_cuts, smooth),
            Action::FlashGuard(on) => set(&mut self.flash_guard, on),
            _ => return None,
        };
        Some(Applied::from_change(changed))
    }
}

fn set<T: PartialEq>(slot: &mut T, new: T) -> bool {
    let changed = *slot != new;
    *slot = new;
    changed
}
