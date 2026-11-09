//! Times `load_preset_file` for a profile's presets, headless (no window):
//!   cargo run --release --example preset_load_time -- deathstep
//!
//! The preview loads a preset inside `App::draw`, on the thread that is about to
//! draw the frame, so whatever this costs is a frame the preview misses. The
//! offline render keeps a virtual clock and cannot drop a frame, so the same
//! cost there only makes the render slower, never jerkier. This example exists
//! to say which of the two the reported `worst` draw actually is.
use projectmp4::gl_context::GlContext;
use projectmp4::presets::{Manifest, Packs};
use std::time::Instant;

fn main() {
    let profile = std::env::args().nth(1).unwrap_or_else(|| "deathstep".to_string());
    let packs = match Packs::ensure(&projectmp4::presets::default_cache_dir().unwrap()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("SKIPPED: no preset packs: {e}");
            return;
        }
    };
    let root = packs.cache_dir().unwrap().to_path_buf();
    let manifest = Manifest::load(std::path::Path::new("presets/profiles.toml")).unwrap();
    let p = manifest.profile(&profile).unwrap_or_else(|| panic!("unknown profile {profile}"));

    // 1920x1080 so the shader compiles are the ones the preview pays for.
    let gl = GlContext::new(1920, 1080).unwrap();
    let pm = gl.create_instance().unwrap();
    pm.set_texture_search_paths(&[packs.textures.as_path()]).unwrap();
    pm.set_window_size(1920, 1080);
    pm.set_mesh_size(64, 48);

    let mut ms: Vec<(f64, String)> = Vec::new();
    for (i, rel) in p.presets.iter().enumerate() {
        let path = root.join(rel);
        let t = Instant::now();
        let r = pm.load_preset_file(&path, true);
        let took = t.elapsed().as_secs_f64() * 1000.0;
        // A load is lazy in places; render one frame so the compile is paid here.
        let t2 = Instant::now();
        pm.render_frame_fbo(gl.fbo());
        let first_frame = t2.elapsed().as_secs_f64() * 1000.0;
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        println!(
            "{:>3}  load {:>8.1} ms  first frame {:>8.1} ms  {}{}",
            i,
            took,
            first_frame,
            &name[..name.len().min(58)],
            if r.is_err() { "   (LOAD FAILED)" } else { "" }
        );
        ms.push((took + first_frame, name));
    }
    ms.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let total: f64 = ms.iter().map(|m| m.0).sum();
    println!("\nload+first-frame: n={} total {:.0} ms  mean {:.1} ms", ms.len(), total, total / ms.len() as f64);
    println!("worst five:");
    for (t, n) in ms.iter().take(5) {
        println!("  {:>8.1} ms  {}", t, &n[..n.len().min(58)]);
    }
}
