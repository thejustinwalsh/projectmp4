# projectmp4

Renders music-visualizer videos from audio using libprojectM 4.2.

Target: macOS / Apple Silicon only.

## Prerequisites

- rustup (Rust toolchain)
- cmake (>= 3.21)
- ffmpeg
- libclang (Xcode command line tools) for bindgen

libprojectM is vendored at `vendor/projectm` (git submodule, pinned to `eae22de4`)
and built statically by `build.rs`. Homebrew's `projectm` is never used.

```
git clone --recurse-submodules <repo>
cargo test
```
