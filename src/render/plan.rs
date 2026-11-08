//! Everything decided before the first pixel: arguments, the resolved title,
//! the switch timeline and the preset order. A [`RenderPlan`] is plain data
//! (it serialises to JSON), so it can be inspected, diffed and handed to a
//! preview without rendering anything.

use crate::audio::analysis::DEFAULT_MODELS;
use crate::audio::normalise::{trim, wav_frames, PIPELINE_RATE};
use crate::audio::sections::clip::{self, ClipKind};
use crate::audio::sections::LABELS;
use crate::audio::timeline::{plan_switches_reporting, Cut, SwitchPoint};
use crate::audio::{analyse_outcome, normalise, Analysis, Driver, Outcome};
use crate::encode::{video_bitrate_kbps, video_bitrate_kbps_fast, Target, AUDIO_KBPS};
use crate::ffi::{Error, Result};
use crate::presets::{default_cache_dir, Manifest, Packs};
use crate::render::MarkStyle;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;

/// Overscan: projectM draws this much larger than the frame we keep, and the
/// centre `WIDTH x HEIGHT` is cropped out of it.
///
/// MilkDrop presets leave junk in the outermost rows and columns — the
/// per-pixel warp mesh samples outside the previous frame's texture at the
/// clip edge, and border and wave primitives are drawn right up against it, so
/// the edge either smears or shows a hard seam. There is nothing to fix inside
/// a preset: it is what the engine does at the boundary. Drawing 5% wider and
/// taller and throwing the margin away puts that boundary off-frame.
///
/// 21/20 of 1920x1080 is 2016x1134, both even, so the crop margin is a whole
/// 48 columns and 27 rows on each side. The cost is the 10% more pixels
/// projectM shades.
pub const RENDER_WIDTH: u32 = WIDTH * 21 / 20;
pub const RENDER_HEIGHT: u32 = HEIGHT * 21 / 20;
pub const DEFAULT_PROFILE: &str = "deathstep";
/// Seconds between ordinary preset changes, and between a drop's hard cuts.
pub const MIN_GAP: f64 = 8.0;
pub const DROP_GAP: f64 = 4.0;
/// Frame rates accepted. Below 10 the half-second silence hold and the
/// one-second dead-preset rule degenerate to a frame or two.
pub const MIN_FPS: u32 = 10;
pub const MAX_FPS: u32 = 240;
/// Clip length `--target x` asks for when `--clip-seconds` is not given: under
/// the 140 s X limit with margin.
pub const X_TARGET_WANT: f64 = 135.0;
/// Fade applied at each cut edge of a clip.
pub const CLIP_FADE: f64 = 1.5;

const PROFILES_TOML: &str = include_str!("../../presets/profiles.toml");

#[derive(Debug, Clone)]
pub struct RenderArgs {
    pub input: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub art: Option<PathBuf>,
    pub profile: String,
    /// Explicit `.milk` files. When non-empty they replace the profile's list.
    pub presets: Vec<PathBuf>,
    pub fps: u32,
    pub watermark_style: MarkStyle,
    /// Watermark does not follow the bass.
    pub static_overlay: bool,
    /// Hardware (VideoToolbox) encode instead of libx264.
    pub fast: bool,
    /// Size goal for the output; resolved to a bitrate before rendering starts.
    pub target: Target,
    /// Limit frame-to-frame luminance change (WCAG 2.3.1). Off unless asked
    /// for: at the rates these presets reach, the guard blends nearly every
    /// frame with its predecessor (measured: engaged on 97% of frames, holding
    /// 99% of the previous frame on the hardest one), which reads as muddy and
    /// flattens the response to the beat.
    pub flash_guard: bool,
    /// Most relative luminance any region may travel per second under the guard.
    pub flash_rate: f32,
    /// Default: `<input stem>.mp4` in the working directory.
    pub out: Option<PathBuf>,
    pub models: PathBuf,
    /// Print progress to stderr.
    pub progress: bool,
    /// Render only the first N seconds (a trial run). `total_frames` reflects it.
    pub limit_seconds: Option<f64>,
    /// Which part of the track to render. Anything but `Full` separates stems
    /// and runs the section model first.
    pub clip: ClipKind,
    /// Clip length in seconds; `None` is the chosen section's own length (135 s under `--target x`).
    pub clip_seconds: Option<f64>,
}

impl RenderArgs {
    pub fn new(input: impl Into<PathBuf>) -> RenderArgs {
        RenderArgs {
            input: input.into(),
            title: None,
            artist: None,
            art: None,
            profile: DEFAULT_PROFILE.into(),
            presets: Vec::new(),
            fps: 60,
            watermark_style: MarkStyle::Word,
            static_overlay: false,
            fast: false,
            target: Target::None,
            flash_guard: false,
            flash_rate: crate::render::flash::DEFAULT_RATE,
            out: None,
            models: PathBuf::from(DEFAULT_MODELS),
            progress: false,
            limit_seconds: None,
            clip: ClipKind::Full,
            clip_seconds: None,
        }
    }

    pub fn out_path(&self) -> PathBuf {
        self.out.clone().unwrap_or_else(|| {
            let stem = self.input.file_stem().unwrap_or_default();
            PathBuf::from(stem).with_extension("mp4")
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderPlan {
    pub input: PathBuf,
    pub title: String,
    pub artist: Option<String>,
    pub art: Option<PathBuf>,
    pub fps: u32,
    /// Seconds rendered: the decoded audio's length, or `limit_seconds` if shorter.
    pub duration: f64,
    /// The trial-render limit this plan was made with, if any.
    pub limit_seconds: Option<f64>,
    /// Which part of the source was rendered.
    pub clip: ClipKind,
    /// The span cut from the source, in the *source's* timeline. `None` for `full`.
    pub clip_span: Option<(f64, f64)>,
    /// `Reason::name()` of the pick, and `LABELS` entry of the section it came from.
    pub clip_reason: Option<String>,
    pub clip_label: Option<String>,
    /// `ceil(duration * fps)`.
    pub total_frames: u64,
    pub bpm: f32,
    /// Overlay and encode settings, recorded so a dumped plan replays as it was previewed.
    pub watermark_style: MarkStyle,
    pub static_overlay: bool,
    pub fast: bool,
    pub target: Target,
    /// Video bitrate the target resolved to for `duration`; `None` is CRF 17.
    pub bitrate_kbps: Option<u32>,
    pub flash_guard: bool,
    pub flash_rate: f32,
    /// Pinned commit of the preset pack the pool was resolved against.
    pub packs_revision: Option<String>,
    pub switches: Vec<SwitchPoint>,
    /// The preset for each segment: the opening one, then one per switch.
    pub preset_sequence: Vec<PathBuf>,
    /// Every candidate, in order. A preset that fails to load is replaced by
    /// the next entry after it, wrapping around.
    pub pool: Vec<PathBuf>,
    /// Directory projectM resolves textures from, when known.
    pub textures: Option<PathBuf>,
}

impl RenderPlan {
    pub fn to_json(&self) -> String {
        let paths = |v: &[PathBuf]| v.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let v = json!({
            "input": self.input.to_string_lossy(),
            "title": self.title,
            "artist": self.artist,
            "art": self.art.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "fps": self.fps,
            "duration": self.duration,
            "limit_seconds": self.limit_seconds,
            "clip": self.clip.name(),
            "clip_span": self.clip_span.map(|(a, b)| [a, b]),
            "clip_reason": self.clip_reason,
            "clip_label": self.clip_label,
            "total_frames": self.total_frames,
            "bpm": self.bpm,
            "watermark_style": match self.watermark_style { MarkStyle::Word => "word", MarkStyle::Sigil => "sigil" },
            "static_overlay": self.static_overlay,
            "fast": self.fast,
            "target": self.target.name(),
            "bitrate_kbps": self.bitrate_kbps,
            "flash_guard": self.flash_guard,
            // Rounded so an f32 like 0.010 reads back as 0.01, not 0.009999999776.
            "flash_rate": (self.flash_rate as f64 * 1e6).round() / 1e6,
            "packs_revision": self.packs_revision,
            "switches": self.switches.iter().map(|s| json!({
                "t": s.t,
                "cut": match s.cut { Cut::Hard => "hard", Cut::Soft => "soft" },
            })).collect::<Vec<_>>(),
            "preset_sequence": paths(&self.preset_sequence),
            "pool": paths(&self.pool),
            "textures": self.textures.as_ref().map(|p| p.to_string_lossy().into_owned()),
        });
        serde_json::to_string_pretty(&v).expect("a json! value always serialises")
    }

    pub fn from_json(s: &str) -> Result<RenderPlan> {
        let v: Value = serde_json::from_str(s).map_err(|e| Error::msg(format!("plan json: {e}")))?;
        let bad = |k: &str| Error::msg(format!("plan json: missing or malformed {k:?}"));
        let str_of = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
        let opt_str = |k: &str| match v.get(k) {
            Some(Value::Null) => Ok(None),
            Some(x) => x.as_str().map(|s| Some(s.to_string())).ok_or_else(|| bad(k)),
            // A field a plan written by an older version lacks is absent, not malformed.
            None => Ok(None),
        };
        let paths = |k: &str| -> Result<Vec<PathBuf>> {
            v.get(k)
                .and_then(Value::as_array)
                .ok_or_else(|| bad(k))?
                .iter()
                .map(|x| x.as_str().map(PathBuf::from).ok_or_else(|| bad(k)))
                .collect()
        };
        let switches = v
            .get("switches")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("switches"))?
            .iter()
            .map(|s| {
                let t = s.get("t").and_then(Value::as_f64).ok_or_else(|| bad("switches.t"))?;
                let cut = match s.get("cut").and_then(Value::as_str) {
                    Some("hard") => Cut::Hard,
                    Some("soft") => Cut::Soft,
                    _ => return Err(bad("switches.cut")),
                };
                Ok(SwitchPoint { t, cut })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(RenderPlan {
            input: str_of("input").map(PathBuf::from).ok_or_else(|| bad("input"))?,
            title: str_of("title").ok_or_else(|| bad("title"))?,
            artist: opt_str("artist")?,
            art: opt_str("art")?.map(PathBuf::from),
            fps: v.get("fps").and_then(Value::as_u64).ok_or_else(|| bad("fps"))? as u32,
            limit_seconds: match v.get("limit_seconds") {
                None | Some(Value::Null) => None,
                Some(x) => Some(x.as_f64().ok_or_else(|| bad("limit_seconds"))?),
            },
            clip: match str_of("clip") {
                None => ClipKind::Full,
                Some(c) => ClipKind::parse(&c).map_err(|_| bad("clip"))?,
            },
            clip_span: match v.get("clip_span") {
                None | Some(Value::Null) => None,
                Some(x) => match x.as_array().map(Vec::as_slice) {
                    Some([a, b]) => Some((a.as_f64().ok_or_else(|| bad("clip_span"))?, b.as_f64().ok_or_else(|| bad("clip_span"))?)),
                    _ => return Err(bad("clip_span")),
                },
            },
            clip_reason: opt_str("clip_reason")?,
            clip_label: opt_str("clip_label")?,
            duration: v.get("duration").and_then(Value::as_f64).ok_or_else(|| bad("duration"))?,
            total_frames: v.get("total_frames").and_then(Value::as_u64).ok_or_else(|| bad("total_frames"))?,
            bpm: v.get("bpm").and_then(Value::as_f64).ok_or_else(|| bad("bpm"))? as f32,
            // Fields added after the first version default when absent, so an
            // older plan still loads. Adding a field must never invalidate a plan.
            watermark_style: match str_of("watermark_style").as_deref() {
                None | Some("word") => MarkStyle::Word,
                Some("sigil") => MarkStyle::Sigil,
                _ => return Err(bad("watermark_style")),
            },
            static_overlay: v.get("static_overlay").and_then(Value::as_bool).unwrap_or(false),
            fast: v.get("fast").and_then(Value::as_bool).unwrap_or(false),
            target: match str_of("target") {
                None => Target::None,
                Some(t) => Target::parse(&t).map_err(|_| bad("target"))?,
            },
            bitrate_kbps: match v.get("bitrate_kbps") {
                None | Some(Value::Null) => None,
                Some(x) => Some(x.as_u64().and_then(|k| u32::try_from(k).ok()).ok_or_else(|| bad("bitrate_kbps"))?),
            },
            // A plan from before the guard existed replays with it on: safe by default.
            flash_guard: v.get("flash_guard").and_then(Value::as_bool).unwrap_or(true),
            flash_rate: match v.get("flash_rate") {
                None | Some(Value::Null) => crate::render::flash::DEFAULT_RATE,
                Some(x) => x.as_f64().ok_or_else(|| bad("flash_rate"))? as f32,
            },
            packs_revision: opt_str("packs_revision")?,
            switches,
            preset_sequence: paths("preset_sequence")?,
            pool: paths("pool")?,
            textures: opt_str("textures")?.map(PathBuf::from),
        })
    }
}

/// Scratch directory removed on drop, so every exit path (including `?`)
/// cleans up.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<TempDir> {
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let p = std::env::temp_dir().join(format!(
            "projectmp4-render-{}-{}-{nanos}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).map_err(|e| Error::msg(format!("create {}: {e}", p.display())))?;
        Ok(TempDir(p))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A plan plus the decoded material it was made from.
pub(crate) struct Prepared {
    pub plan: RenderPlan,
    pub driver: Driver,
    pub wav: PathBuf,
    _tmp: TempDir,
}

/// Normalise, analyse, load the driver, plan the switches and order the presets.
pub fn render_plan(args: &RenderArgs) -> Result<RenderPlan> {
    prepare(args).map(|p| p.plan)
}

pub(crate) fn prepare(args: &RenderArgs) -> Result<Prepared> {
    check_args(args)?;
    let tmp = TempDir::new()?;
    let src = normalise(&args.input, &tmp.0)?;
    prepare_with(args, tmp, src)
}

/// Plan an already-normalised source. Public so a test can hand it a WAV that
/// `normalise` itself would never produce (a header with no samples) and watch
/// `prepare`'s own call to `require_samples` refuse it.
pub fn render_plan_from_source(args: &RenderArgs, src: crate::audio::Source) -> Result<RenderPlan> {
    prepare_with(args, TempDir::new()?, src).map(|p| p.plan)
}

fn prepare_with(args: &RenderArgs, tmp: TempDir, mut src: crate::audio::Source) -> Result<Prepared> {
    check_args(args)?;
    let frames = require_samples(wav_frames(&src.wav)?, &args.input)?;
    let mut wav_secs = frames as f64 / PIPELINE_RATE as f64;
    // A clip is cut from the WAV before anything else looks at it, so beat
    // analysis, the driver, the switches, the loop and the encoder all see the
    // clip as if it were the whole track. The full track is analysed once here,
    // only for its downbeats: the grid the cut snaps to.
    let mut clip_made = None;
    if args.clip != ClipKind::Full {
        let want = args.clip_seconds.or((args.target == Target::X).then_some(X_TARGET_WANT));
        if let Some(w) = want {
            check_clip_target(args.target, w, wav_secs)?;
        }
        let downbeats = match analyse_outcome(&src, &args.models, Some(wav_secs))? {
            Outcome::Beats(a) => a.downbeats,
            Outcome::NoBeats(why) => {
                eprintln!("clip: {why}; the cut will not be snapped to a bar");
                Vec::new()
            }
        };
        let chosen = clip::locate(&src.wav, args.clip, want, &downbeats, wav_secs, &tmp.0, args.progress)?;
        src = trim(&src, chosen.start, chosen.end, CLIP_FADE, &tmp.0)?;
        let frames = require_samples(wav_frames(&src.wav)?, &args.input)?;
        wav_secs = frames as f64 / PIPELINE_RATE as f64;
        clip_made = Some(chosen);
    }
    // Fail before the minutes of analysis and the hours of rendering: an
    // impossible size target is known from the length alone.
    let shown = args.limit_seconds.map_or(wav_secs, |l| l.min(wav_secs));
    // The two encoders size differently: only libx264's constant-quality mode
    // has a measured cost that a capped target can be trusted to fit.
    let bitrate_kbps = if args.fast {
        video_bitrate_kbps_fast(args.target, shown, AUDIO_KBPS)?
    } else {
        video_bitrate_kbps(args.target, shown, AUDIO_KBPS)?
    };
    // Beatless music (an ambient pad) gives the analyser nothing to find. That
    // is not a reason to refuse the track: switch on a fixed grid instead.
    // That applies only to an analyser that ran and found no beats. One that
    // could not run (missing or broken models) is an error the user can fix:
    // falling back would hide it for good. Stretches the analysis leaves
    // without downbeats are filled the same way by `plan_switches_reporting`.
    let (analysis, beatless) = match analyse_outcome(&src, &args.models, Some(wav_secs))? {
        Outcome::Beats(a) => (a, false),
        Outcome::NoBeats(why) => {
            eprintln!("render: {why}; switching presets on a fixed {MIN_GAP} s grid");
            (Analysis { bpm: 0.0, beats: Vec::new(), downbeats: Vec::new(), duration: wav_secs }, true)
        }
    };
    let driver = Driver::load(&src)?;
    let (mut switches, fill) = plan_switches_reporting(&analysis, &driver, MIN_GAP, DROP_GAP);
    if !beatless {
        if let Some(n) = fill.notice(wav_secs) {
            eprintln!("render: {n}");
        }
    }

    let non_empty = |s: &Option<String>| s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let stem = args.input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let title = non_empty(&args.title).or_else(|| src.title.clone()).unwrap_or(stem);
    let artist = non_empty(&args.artist).or_else(|| src.artist.clone());

    let (pool, textures) = resolve_pool(args)?;

    // The audio the encoder muxes (and `-shortest` trims to) is the normalised
    // WAV, and the driver is decoded from that same WAV, so its length sets the
    // video length.
    // The tiny subtraction keeps an exact multiple (12.0 s * 60) from rounding up a frame.
    let total_frames = (shown * args.fps as f64 - 1e-9).ceil().max(1.0) as u64;
    // A switch the render never reaches is not part of the plan.
    switches.retain(|s| s.t < shown);
    let preset_sequence: Vec<PathBuf> =
        (0..=switches.len()).map(|i| pool[i % pool.len()].clone()).collect();
    let plan = RenderPlan {
        input: args.input.clone(),
        title,
        artist,
        art: args.art.clone(),
        fps: args.fps,
        duration: shown,
        limit_seconds: args.limit_seconds,
        clip: args.clip,
        clip_span: clip_made.as_ref().map(|c| (c.start, c.end)),
        clip_reason: clip_made.as_ref().map(|c| c.reason.name().to_string()),
        clip_label: clip_made.as_ref().and_then(|c| c.label).map(|l| LABELS[l as usize].to_string()),
        total_frames,
        bpm: analysis.bpm,
        watermark_style: args.watermark_style,
        static_overlay: args.static_overlay,
        fast: args.fast,
        target: args.target,
        bitrate_kbps,
        flash_guard: args.flash_guard,
        flash_rate: args.flash_rate,
        // Only a pool resolved from the packs has a pack revision to record.
        packs_revision: if args.presets.is_empty() {
            crate::presets::packs::sources()
                .ok()
                .and_then(|v| v.into_iter().find(|s| s.name == "presets-cream-of-the-crop"))
                .map(|s| s.commit)
        } else {
            None
        },
        switches,
        preset_sequence,
        pool,
        textures,
    };
    Ok(Prepared { plan, driver, wav: src.wav, _tmp: tmp })
}

/// The candidate presets and the texture directory.
fn resolve_pool(args: &RenderArgs) -> Result<(Vec<PathBuf>, Option<PathBuf>)> {
    let packs = default_cache_dir().and_then(|c| Packs::ensure(&c));
    if !args.presets.is_empty() {
        let textures = match packs {
            Ok(p) => Some(p.textures),
            Err(e) => {
                eprintln!("render: preset packs unavailable ({e}); textures will not resolve");
                None
            }
        };
        return Ok((args.presets.clone(), textures));
    }
    let packs = packs.map_err(|e| Error::msg(format!("preset packs unavailable (run `projectmp4 init`): {e}")))?;
    let manifest = Manifest::parse(PROFILES_TOML)?;
    let profile = manifest.profile(&args.profile).ok_or_else(|| {
        let known: Vec<&str> = manifest.profiles.iter().map(|p| p.name.as_str()).collect();
        Error::msg(format!("unknown profile {:?}; known: {}", args.profile, known.join(", ")))
    })?;
    if profile.presets.is_empty() {
        return Err(Error::msg(format!("profile {:?} lists no presets", profile.name)));
    }
    let pool = profile.presets.iter().map(|p| packs.cotc.join(p)).collect();
    Ok((pool, Some(packs.textures)))
}

/// The flash rate is relative luminance per second. The guard bounds flashes
/// per second by `rate / 0.20` (a flash needs 0.10 of travel each way), so 0.60
/// is the exact WCAG threshold (3.0 per second); the ceiling is 0.54, which
/// leaves 10% headroom.
pub fn check_flash_rate(r: f32) -> Result<()> {
    use crate::render::flash::{DEFAULT_RATE, MAX_RATE};
    if r.is_finite() && (0.001..MAX_RATE).contains(&r) {
        Ok(())
    } else {
        Err(Error::msg(format!(
            "--flash-rate must be at least 0.001 and below {MAX_RATE} (measured default {DEFAULT_RATE}), got {r}: \
             a rate of 0.60/s permits exactly 0.60/0.20 = 3.0 flashes per second, which is the WCAG \
             threshold itself; below {MAX_RATE} leaves 10% headroom under it"
        )))
    }
}

/// A trial-render limit must be a positive, finite number of seconds: anything
/// else would silently mean "the whole track".
pub fn check_limit(l: f64) -> Result<()> {
    if l.is_finite() && l > 0.0 {
        Ok(())
    } else {
        Err(Error::msg(format!("--limit-seconds must be a positive number of seconds, got {l}")))
    }
}

/// Every check that needs only the arguments, so it can run before the decode.
/// Both entry points call it: `prepare` to refuse before ffmpeg starts,
/// `prepare_with` because a caller may hand it a source directly.
fn check_args(args: &RenderArgs) -> Result<()> {
    if !(MIN_FPS..=MAX_FPS).contains(&args.fps) {
        return Err(Error::msg(format!("--fps {} is out of range {MIN_FPS}..={MAX_FPS}", args.fps)));
    }
    check_flash_rate(args.flash_rate)?;
    if let Some(l) = args.limit_seconds {
        check_limit(l)?;
    }
    if let Some(c) = args.clip_seconds {
        check_clip_seconds(c)?;
        if args.clip == ClipKind::Full {
            return Err(Error::msg(format!(
                "--clip-seconds {c} has no effect with --clip full, which renders the whole track; \
                 pass --clip drop or --clip intro, or use --limit-seconds {c} to render the first {c} s"
            )));
        }
    }
    Ok(())
}

/// Validate the clip length *requested* against the target, before any stem work.
///
/// This checks the request, not the outcome. Asking for more seconds than the
/// track holds is legal and clamps to the track, hence the `min`; asking for
/// more seconds than the target accepts is not, and is better said now than
/// after minutes of stem separation. A clip shorter than `want.min(track_secs)`
/// can still pass a target this refuses: it is not a prediction of the result.
pub fn check_clip_target(target: Target, want: f64, track_secs: f64) -> Result<()> {
    video_bitrate_kbps(target, want.min(track_secs), AUDIO_KBPS).map(|_| ())
}

/// A clip length must be a positive, finite number of seconds.
pub fn check_clip_seconds(v: f64) -> Result<()> {
    if v.is_finite() && v > 0.0 {
        Ok(())
    } else {
        Err(Error::msg(format!("--clip-seconds must be a positive number of seconds, got {v}")))
    }
}

/// An input that decodes to nothing (ID3 tags only, a header-only truncation)
/// would otherwise become a one-frame video and say nothing.
fn require_samples(frames: u64, input: &std::path::Path) -> Result<u64> {
    if frames == 0 {
        return Err(Error::msg(format!("{} decoded to no audio samples; there is nothing to render", input.display())));
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decode_with_no_samples_is_an_error_naming_the_input() {
        // `normalise` already refuses most such inputs (ffprobe reports no duration for
        // them), so the only way to reach this is a file with a duration and no samples,
        // which ffmpeg cannot be made to produce from a fixture: tested at the seam.
        let e = require_samples(0, std::path::Path::new("ambient.mp3")).unwrap_err().to_string();
        assert!(e.contains("ambient.mp3") && e.contains("no audio samples"), "{e}");
        assert_eq!(require_samples(1, std::path::Path::new("x")).unwrap(), 1);
    }
}
