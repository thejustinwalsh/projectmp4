use std::path::Path;
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    let result = if args.len() == 2 {
        deck_media_codec::loudness::scan(Path::new(&args[1]))
    } else { Err("Usage: deck-media-codec SUNO_CACHE_FILE".into()) };
    match result {
        Ok(value) => println!("{}", value),
        Err(error) => { eprintln!("Loudness scan: {error}"); std::process::exit(1); }
    }
}
