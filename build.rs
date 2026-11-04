use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor/projectm/src");
    println!("cargo:rerun-if-changed=vendor/projectm/CMakeLists.txt");

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

    println!("cargo:rustc-link-search=native={}", dst.join("lib").display());
    // projectM_eval and glad are not installed by cmake; link them from the build tree.
    let bt = dst.join("build");
    println!("cargo:rustc-link-search=native={}", bt.join("vendor/projectm-eval/projectm-eval").display());
    println!("cargo:rustc-link-search=native={}", bt.join("vendor/glad").display());
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
        .header(include.join("projectM-4/render_opengl.h").display().to_string())
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
