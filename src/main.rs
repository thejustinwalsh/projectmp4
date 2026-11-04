use projectmp4::presets::{default_cache_dir, Manifest, Packs};
use std::path::PathBuf;

const PRESETS_PER_PROFILE: usize = 40;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("rank") => run_rank(args.get(1).map(PathBuf::from)),
        _ => {
            eprintln!("usage: projectmp4 rank [OUT.toml]   (default OUT: presets/profiles.toml)");
            2
        }
    };
    std::process::exit(code);
}

/// Fetch the pinned packs, rank every profile and write the manifest.
fn run_rank(out: Option<PathBuf>) -> i32 {
    let out = out.unwrap_or_else(|| PathBuf::from("presets/profiles.toml"));
    let result = default_cache_dir()
        .and_then(|cache| Packs::ensure(&cache))
        .and_then(|packs| Manifest::generate(&packs, PRESETS_PER_PROFILE))
        .and_then(|m| m.save(&out).map(|()| m));
    match result {
        Ok(m) => {
            for p in &m.profiles {
                println!("{:<10} {} presets", p.name, p.presets.len());
            }
            println!("wrote {}", out.display());
            0
        }
        Err(e) => {
            eprintln!("rank: {e}");
            1
        }
    }
}
