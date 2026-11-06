//! Command line: `init`, `rank`, `preview`, `render`.

use crate::encode::Target;
use crate::ffi::{Error, Result};
use crate::presets::{default_cache_dir, Manifest, Packs};
use crate::render::plan::{render_plan, RenderArgs};
use crate::render::r#loop::render;
use crate::render::MarkStyle;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

const PRESETS_PER_PROFILE: usize = 40;

#[derive(Parser)]
#[command(name = "projectmp4", about = "Turn an audio file into a 1080p60 music-visualiser mp4")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Fetch the pinned preset packs into the cache.
    Init,
    /// Rank every profile and write the manifest.
    Rank {
        /// Output file.
        #[arg(default_value = "presets/profiles.toml")]
        out: PathBuf,
    },
    /// Play a track in a window with the presets and switch times `render` will use.
    ///
    /// Right/Left step to the next/previous preset (off the schedule until the
    /// next planned switch), L locks the preset on screen, P prints its path
    /// (pass it to `render --preset`), Esc quits.
    Preview(PlanCmd),
    /// Render an audio file to mp4.
    Render(RenderCmd),
}

/// What decides the plan: the same flags for `render` and `preview`, so the
/// preview of a track is the render of it.
#[derive(Args)]
struct PlanCmd {
    input: PathBuf,
    #[arg(long, default_value = "deathstep")]
    profile: String,
    /// Use this .milk file instead of the profile's list; repeatable.
    #[arg(long)]
    preset: Vec<PathBuf>,
    /// Frames per second, 10 to 240. MilkDrop presets step their equations once per
    /// frame and were authored for about 60 fps, so motion runs roughly 6x slower at
    /// 10 fps and roughly 4x faster at 240 (direction and rough size, not measured).
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(10..=240))]
    fps: u32,
    /// Use only the first N seconds.
    #[arg(long, value_parser = parse_limit)]
    limit_seconds: Option<f64>,
    /// Defaults to the file's title tag, then its file name.
    #[arg(long)]
    title: Option<String>,
    /// Defaults to the file's artist tag.
    #[arg(long)]
    artist: Option<String>,
    /// Cover art (jpeg or png), shown beside the title.
    #[arg(long)]
    art: Option<PathBuf>,
    /// Watermark artwork: word or sigil.
    #[arg(long, default_value = "word", value_parser = parse_style)]
    watermark_style: MarkStyle,
    /// Watermark does not pulse with the bass.
    #[arg(long)]
    static_overlay: bool,
    /// Do not limit frame-to-frame luminance change. The limiter keeps the
    /// output under the WCAG 2.3.1 photosensitive-seizure flash threshold.
    #[arg(long)]
    no_flash_guard: bool,
    /// Most relative luminance (fraction of maximum) any region may travel per
    /// second under the flash guard, below 0.54 (0.60 is the exact WCAG threshold). Flashes per second are bounded
    /// by rate / 0.20 at every fps; the 0.48 default gives 2.4.
    #[arg(long, default_value_t = crate::render::flash::DEFAULT_RATE, value_parser = parse_flash_rate)]
    flash_rate: f32,
}

impl PlanCmd {
    fn args(self) -> RenderArgs {
        let mut a = RenderArgs::new(self.input);
        a.profile = self.profile;
        a.presets = self.preset;
        a.fps = self.fps;
        a.limit_seconds = self.limit_seconds;
        a.title = self.title;
        a.artist = self.artist;
        a.art = self.art;
        a.watermark_style = self.watermark_style;
        a.static_overlay = self.static_overlay;
        a.flash_guard = !self.no_flash_guard;
        a.flash_rate = self.flash_rate;
        a
    }
}

#[derive(Args)]
struct RenderCmd {
    #[command(flatten)]
    plan: PlanCmd,
    /// Hardware encode (VideoToolbox): faster, larger.
    #[arg(long)]
    fast: bool,
    /// Fit the output to where it is going: none, discord (500 MB), x (512 MB, 140 s)
    /// or youtube. Capped targets use a single-pass bitrate sized from the track length.
    #[arg(long, default_value = "none", value_parser = parse_target)]
    target: Target,
    /// Output path. Default: <input name>.mp4 in the current directory.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Write the render plan as JSON to this path and exit without rendering.
    #[arg(long)]
    dump_plan: Option<PathBuf>,
}

fn parse_limit(s: &str) -> std::result::Result<f64, String> {
    let v: f64 = s.parse().map_err(|_| format!("{s:?} is not a number"))?;
    crate::render::plan::check_limit(v).map_err(|e| e.0.into_owned())?;
    Ok(v)
}

fn parse_flash_rate(s: &str) -> std::result::Result<f32, String> {
    let v: f32 = s.parse().map_err(|_| format!("{s:?} is not a number"))?;
    crate::render::plan::check_flash_rate(v).map_err(|e| e.0.into_owned())?;
    Ok(v)
}

fn parse_target(s: &str) -> std::result::Result<Target, String> {
    Target::parse(s).map_err(|e| e.0.into_owned())
}

fn parse_style(s: &str) -> std::result::Result<MarkStyle, String> {
    match s {
        "word" => Ok(MarkStyle::Word),
        "sigil" => Ok(MarkStyle::Sigil),
        _ => Err("expected word or sigil".into()),
    }
}

/// Parse `std::env::args`, run, and return the process exit code.
pub fn run() -> i32 {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Init => init(),
        Cmd::Rank { out } => rank(&out),
        Cmd::Preview(c) => crate::preview::preview(&c.args()),
        Cmd::Render(c) => render_cmd(c),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn init() -> Result<()> {
    let packs = Packs::ensure(&default_cache_dir()?)?;
    println!("presets ready at {}", packs.cotc.display());
    Ok(())
}

fn rank(out: &std::path::Path) -> Result<()> {
    let packs = Packs::ensure(&default_cache_dir()?)?;
    let (m, stats) = Manifest::generate_report(&packs, PRESETS_PER_PROFILE)?;
    m.save(out)?;
    for (p, s) in m.profiles.iter().zip(&stats) {
        println!(
            "{:<10} {} presets (short {}); eligible {}, examined {} ({} cached, {} rendered), passed {}, over ceiling {}, unloadable {}",
            p.name,
            p.presets.len(),
            p.shortfall,
            s.eligible,
            s.examined,
            s.cache_hits,
            s.rendered,
            s.passed,
            s.rejected.len(),
            s.unloadable.len()
        );
    }
    println!("wrote {}", out.display());
    Ok(())
}

fn render_cmd(c: RenderCmd) -> Result<()> {
    let mut a = c.plan.args();
    a.fast = c.fast;
    a.target = c.target;
    a.out = c.out;
    a.progress = true;
    if let Some(path) = c.dump_plan {
        let plan = render_plan(&a)?;
        std::fs::write(&path, plan.to_json()).map_err(|e| Error::msg(format!("write {}: {e}", path.display())))?;
        println!("wrote {}", path.display());
        return Ok(());
    }
    let out = render(&a)?;
    println!("wrote {}", out.display());
    Ok(())
}
