use deck_track_analysis::scheduling::{analyze_resumable, Schedule, Yielded};
use std::{
    env,
    path::{Path, PathBuf},
};
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().collect();
    if args.len() < 3 {
        return Err("Usage: deck-track-analysis MODELS_DIRECTORY SUNO_CACHE_FILE [--checkpoint-dir DIR] [--slice-ms N] [--max-units N] [--cancel-file PATH] [--output PATH]".into());
    }
    let mut schedule = Schedule::default();
    let mut checkpoint = None;
    let mut output = None;
    let mut index = 3;
    while index < args.len() {
        let name = args[index].to_str().ok_or("Invalid option")?;
        let value = args.get(index + 1).ok_or("Missing option value")?;
        match name {
            "--checkpoint-dir" => checkpoint = Some(PathBuf::from(value)),
            "--cancel-file" => schedule.cancel_file = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--slice-ms" => schedule.slice_ms = value.to_str().ok_or("Invalid slice")?.parse()?,
            "--max-units" => {
                schedule.max_units = Some(value.to_str().ok_or("Invalid unit budget")?.parse()?)
            }
            _ => return Err(format!("Unknown option {name}").into()),
        }
        index += 2;
    }
    let models = Path::new(&args[1]);
    let input = Path::new(&args[2]);
    let value = if let Some(dir) = checkpoint {
        analyze_resumable(models, input, &dir, &schedule)?
    } else if args.len() == 3 {
        deck_track_analysis::analyze(models, input)?
    } else {
        return Err("Scheduling/output options require --checkpoint-dir".into());
    };
    let bytes = serde_json::to_vec(&value)?;
    if let Some(path) = output {
        use std::io::Write;
        // One process owns an output path. Publish only complete JSON.
        let temporary = path.with_extension(format!("{}.pending", std::process::id()));
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(temporary, &path)?;
    }
    println!("{}", value);
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        if let Some(yielded) = error.downcast_ref::<Yielded>() {
            println!(
                "{}",
                serde_json::json!({"status":"yielded","reason":yielded.0})
            );
            std::process::exit(75);
        }
        eprintln!("Track analysis: {error}");
        std::process::exit(1);
    }
}
