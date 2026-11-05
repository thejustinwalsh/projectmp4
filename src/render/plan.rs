//! Everything decided before the first pixel: arguments, the resolved title,
//! the switch timeline and the preset order. A [`RenderPlan`] is plain data
//! (it serialises to JSON), so it can be inspected, diffed and handed to a
//! preview without rendering anything.

use crate::audio::analysis::DEFAULT_MODELS;
use crate::audio::timeline::{plan_switches, Cut, SwitchPoint};
use crate::audio::{analyse, normalise, Driver, DriverKind};
use crate::ffi::{Error, Result};
use crate::presets::{default_cache_dir, Manifest, Packs};
use crate::render::MarkStyle;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;
pub const DEFAULT_PROFILE: &str = "deathstep";
/// Seconds between ordinary preset changes, and between a drop's hard cuts.
pub const MIN_GAP: f64 = 8.0;
pub const DROP_GAP: f64 = 4.0;

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
    /// `None` prefers stems and falls back to the mix.
    pub driver: Option<DriverKind>,
    pub watermark_style: MarkStyle,
    /// Watermark does not follow the bass.
    pub static_overlay: bool,
    /// Hardware (VideoToolbox) encode instead of libx264.
    pub fast: bool,
    /// Default: `<input stem>.mp4` in the working directory.
    pub out: Option<PathBuf>,
    pub models: PathBuf,
    /// Print progress to stderr.
    pub progress: bool,
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
            driver: None,
            watermark_style: MarkStyle::Word,
            static_overlay: false,
            fast: false,
            out: None,
            models: PathBuf::from(DEFAULT_MODELS),
            progress: false,
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
    /// Length of the decoded audio the video is laid over.
    pub duration: f64,
    /// `ceil(duration * fps)`.
    pub total_frames: u64,
    pub bpm: f32,
    pub driver: DriverKind,
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
            "total_frames": self.total_frames,
            "bpm": self.bpm,
            "driver": match self.driver { DriverKind::Stems => "stems", DriverKind::Mix => "mix" },
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
            None => Err(bad(k)),
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
            duration: v.get("duration").and_then(Value::as_f64).ok_or_else(|| bad("duration"))?,
            total_frames: v.get("total_frames").and_then(Value::as_u64).ok_or_else(|| bad("total_frames"))?,
            bpm: v.get("bpm").and_then(Value::as_f64).ok_or_else(|| bad("bpm"))? as f32,
            driver: match str_of("driver").as_deref() {
                Some("stems") => DriverKind::Stems,
                Some("mix") => DriverKind::Mix,
                _ => return Err(bad("driver")),
            },
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
    if args.fps == 0 || args.fps > 240 {
        return Err(Error::msg(format!("--fps {} is out of range 1..=240", args.fps)));
    }
    let tmp = TempDir::new()?;
    let src = normalise(&args.input, &tmp.0)?;
    let analysis = analyse(&src, &args.models)?;
    let driver = Driver::load(&src, args.driver)?;
    let switches = plan_switches(&analysis, &driver, MIN_GAP, DROP_GAP);

    let non_empty = |s: &Option<String>| s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let stem = args.input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let title = non_empty(&args.title).or_else(|| src.title.clone()).unwrap_or(stem);
    let artist = non_empty(&args.artist).or_else(|| src.artist.clone());

    let (pool, textures) = resolve_pool(args)?;
    let preset_sequence: Vec<PathBuf> =
        (0..=switches.len()).map(|i| pool[i % pool.len()].clone()).collect();

    let duration = driver.frames() as f64 / driver.rate as f64;
    // The tiny subtraction keeps an exact multiple (12.0 s * 60) from rounding up a frame.
    let total_frames = (duration * args.fps as f64 - 1e-9).ceil().max(1.0) as u64;
    let plan = RenderPlan {
        input: args.input.clone(),
        title,
        artist,
        art: args.art.clone(),
        fps: args.fps,
        duration,
        total_frames,
        bpm: analysis.bpm,
        driver: driver.kind,
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
