# projectmp4

Renders music-visualizer videos from audio using libprojectM 4.2.

Target: macOS / Apple Silicon only.

## Prerequisites

- rustup (Rust toolchain)
- cmake (>= 3.21)
- ffmpeg
- libclang (Xcode command line tools) for bindgen

libprojectM is vendored at `vendor/projectm`, copied in from upstream
`eae22de4` rather than referenced as a submodule, and built statically by
`build.rs`. Homebrew's `projectm` is never used. We patch the renderer, so the
source has to be ours to hold — see `vendor/projectm/VENDORED.md` for the
provenance and every local change.

```
git clone <repo>
cd projectmp4
./scripts/fetch-models.sh
cargo test --no-fail-fast
```

`fetch-models.sh` downloads the two beat-tracking ONNX models into
`crates/track-analysis/models`, pinned to an exact upstream commit and verified
by sha256. They are gitignored: this repository carries what is needed to build
it, and 10 MB of third-party weights nobody edits is a dependency to fetch, not
source to commit. Run it once after cloning. It is idempotent, so running it
again is free.

Two further pieces are fetched rather than committed, for the same reason:

- **Preset packs** — `cargo run --release -- init` clones the pinned community
  packs into the cache. `presets/profiles.toml` records which presets were
  chosen; the packs themselves are not in the repository.
- **`vendor/stems-runtime`** — the stem-separation runtime, needed only for
  `--clip drop` and `--clip intro` and for the stem-parity test. Everything
  else builds and runs without it, and the tests that need it skip when it is
  absent.

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
