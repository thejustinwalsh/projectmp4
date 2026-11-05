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
cargo test --no-fail-fast
```

### Running the full gate

`cargo test` alone is **not** the whole suite. Eight tests covering the
community-ranked preset manifest and the per-genre selection profiles are
marked `#[ignore]` because they clone a preset-pack repository over the
network. They are the only coverage the preset-selection pipeline has,
including the only check that `presets/profiles.toml` has not gone stale
against the ranked manifest. Both commands must be green:

```
cargo test --no-fail-fast
```

```
cargo test --test packs --test rank -- --ignored
```

Use `--no-fail-fast`: plain `cargo test` stops at the first failing test
binary and hides the rest.
