use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=scripts/fetch-models.sh");
    fetch_beat_models();

    println!("cargo:rerun-if-changed=vendor/projectm/src");
    println!("cargo:rerun-if-changed=vendor/projectm/CMakeLists.txt");
    println!("cargo:rerun-if-changed=vendor/projectm/vendor/projectm-eval");
    println!("cargo:rerun-if-changed=vendor/projectm/vendor/glad");

    let dst = cmake::Config::new("vendor/projectm")
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("ENABLE_PLAYLIST", "ON")
        .define("ENABLE_SYSTEM_PROJECTM_EVAL", "OFF")
        .define("ENABLE_SYSTEM_GLM", "OFF")
        .define("ENABLE_DEBUG_POSTFIX", "OFF")
        .define("ENABLE_SDL_UI", "OFF")
        .define("ENABLE_INSTALL", "ON")
        .define("BUILD_TESTING", "OFF")
        .define("BUILD_DOCS", "OFF")
        .build();

    println!(
        "cargo:rustc-link-search=native={}",
        dst.join("lib").display()
    );
    // projectM_eval and glad are not installed by cmake; link them from the build tree.
    let bt = dst.join("build");
    println!(
        "cargo:rustc-link-search=native={}",
        bt.join("vendor/projectm-eval/projectm-eval").display()
    );
    println!(
        "cargo:rustc-link-search=native={}",
        bt.join("vendor/glad").display()
    );
    println!("cargo:rustc-link-lib=static=projectM-4");
    println!("cargo:rustc-link-lib=static=projectM-4-playlist");
    println!("cargo:rustc-link-lib=static=projectM_eval");
    println!("cargo:rustc-link-lib=static=glad");
    println!("cargo:rustc-link-lib=c++");
    println!("cargo:rustc-link-lib=framework=OpenGL");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");

    let include = dst.join("include");
    let bindings = bindgen::Builder::default()
        .header(include.join("projectM-4/projectM.h").display().to_string())
        .header(
            include
                .join("projectM-4/render_opengl.h")
                .display()
                .to_string(),
        )
        .clang_arg(format!("-I{}", include.display()))
        .allowlist_function("projectm_.*")
        .allowlist_type("projectm_.*")
        .allowlist_var("PROJECTM_.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("bindgen failed on projectM-4 C headers");
    bindings
        .write_to_file(PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings.rs"))
        .expect("write bindings");
}

/// Fetch the beat-tracking weights if they are not already here.
///
/// They are third-party ONNX exports, pinned by commit and sha256 and fetched
/// rather than committed. Nothing in this crate runs without them — every
/// switch time in a render comes off the beat grid they produce — so a build
/// that leaves them missing only moves the failure to the first render. Doing
/// it here means a fresh clone needs no setup step at all.
///
/// Costs nothing on a normal build: the script hashes what is present and
/// exits. The 62 MB separation model is deliberately *not* fetched here; only
/// `--clip drop` and `--clip intro` need it, and those fetch it on first use.
fn fetch_beat_models() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let models = root.join("crates/track-analysis/models");
    let have = ["beat_this_small.onnx", "mel_spectrogram.onnx"]
        .iter()
        .all(|f| models.join(f).is_file());
    if have {
        return;
    }
    let script = root.join("scripts/fetch-models.sh");
    println!("cargo:warning=fetching the beat-tracking models (11 MB, once)");
    let status = std::process::Command::new("bash")
        .arg(&script)
        .arg("beat")
        .current_dir(&root)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!(
            "{} exited {s}.\nThe beat models are required to build. Run it yourself to see why, \nor set them up by hand from the pins inside it.",
            script.display()
        ),
        Err(e) => panic!(
            "cannot run {}: {e}\nThe beat models are required to build; fetch them with that script.",
            script.display()
        ),
    }
}
