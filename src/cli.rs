//! Command line: `init`, `rank`, `preview`, `render`.

use crate::audio::DriverKind;
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
    /// Live preview window (not implemented yet).
    Preview,
    /// Render an audio file to mp4.
    Render(RenderCmd),
}

#[derive(Args)]
struct RenderCmd {
    input: PathBuf,
    /// Defaults to the file's title tag, then its file name.
    #[arg(long)]
    title: Option<String>,
    /// Defaults to the file's artist tag.
    #[arg(long)]
    artist: Option<String>,
    /// Cover art (jpeg or png), shown beside the title.
    #[arg(long)]
    art: Option<PathBuf>,
    #[arg(long, default_value = "deathstep")]
    profile: String,
    /// Use this .milk file instead of the profile's list; repeatable.
    #[arg(long)]
    preset: Vec<PathBuf>,
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// Audio that drives the visuals: stems (falls back to mix) or mix.
    #[arg(long, value_parser = parse_driver)]
    driver: Option<DriverKind>,
    /// Watermark artwork: word or sigil.
    #[arg(long, default_value = "word", value_parser = parse_style)]
    watermark_style: MarkStyle,
    /// Watermark does not pulse with the bass.
    #[arg(long)]
    static_overlay: bool,
    /// Hardware encode (VideoToolbox): faster, larger.
    #[arg(long)]
    fast: bool,
    /// Output path. Default: <input name>.mp4 in the current directory.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Render only the first N seconds.
    #[arg(long)]
    limit_seconds: Option<f64>,
    /// Write the render plan as JSON to this path and exit without rendering.
    #[arg(long)]
    dump_plan: Option<PathBuf>,
}

fn parse_driver(s: &str) -> std::result::Result<DriverKind, String> {
    match s {
        "stems" => Ok(DriverKind::Stems),
        "mix" => Ok(DriverKind::Mix),
        _ => Err("expected stems or mix".into()),
    }
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
        Cmd::Preview => Err(Error::msg("preview is not implemented yet")),
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
    let m = Manifest::generate(&packs, PRESETS_PER_PROFILE)?;
    m.save(out)?;
    for p in &m.profiles {
        println!("{:<10} {} presets", p.name, p.presets.len());
    }
    println!("wrote {}", out.display());
    Ok(())
}

fn render_cmd(c: RenderCmd) -> Result<()> {
    let mut a = RenderArgs::new(c.input);
    a.title = c.title;
    a.artist = c.artist;
    a.art = c.art;
    a.profile = c.profile;
    a.presets = c.preset;
    a.fps = c.fps;
    a.driver = c.driver;
    a.watermark_style = c.watermark_style;
    a.static_overlay = c.static_overlay;
    a.fast = c.fast;
    a.out = c.out;
    a.progress = true;
    a.limit_seconds = c.limit_seconds;
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
