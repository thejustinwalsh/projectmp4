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
PROJECTMP4_NO_PARITY_ORACLE=1 cargo test --no-fail-fast
```

The environment variable is explained under [Running the full
gate](#running-the-full-gate); a clone needs it, and nothing else.

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

### Test fixtures

The suites build the audio they need, so a clone needs no audio to test
against. `tests/common/tmp.rs` synthesises a click track with ffmpeg, once per
process into `target/tmp`, out of three layers:

| layer | period | why it is there |
| --- | --- | --- |
| 55 Hz kick | every beat | the tempo |
| 3 kHz hat | every eighth | the phase to lock to |
| 38 Hz accent | every 4 beats | the bar line |

It is a click rather than a tone because the first thing the pipeline does is
look for a beat, and a steady tone has no grid to find. The accent is the
layer that is easy to leave out and should not be: with four identical kicks
to a bar the analyser has no bar to find and returns *every* beat as a
downbeat — 60 of 60 over 30 s, measured — so every switch-planning test ends
up exercising a degenerate grid.

A fixture carries its own ground truth, which makes for a stronger assertion
than a real track does: the analyser recovers 120.000 BPM from the 120 BPM
fixture, so a test can pin the answer instead of bracketing it. Four caveats
are measured and recorded on `click_file`:

- It quantises to a ~0.02 s hop grid, so a beat period off that grid cannot
  come back exact — 140 BPM measures 142.86.
- It can pick the wrong metrical level — 100 and 75 both measure double,
  having heard the hat train as the beat.
- **Use 120 or 150 BPM unless you measure another tempo first.** Those two
  measure 120.000 and 150.0001.
- Assert that downbeats are a *subset* of beats, never an exact bar count.
  60 s at 120 BPM recovers the meter exactly (120 beats, 30 downbeats); 12 s
  at 120 BPM does not (24 beats, 17 downbeats). A short fixture proves a
  subset and not a meter.

`samples/` is gitignored. It holds the maintainer's own unreleased music,
which is not ours to redistribute, so a clone has none of it. The few tests
whose claim is about that specific audio — an ID3 tag, one file's hash, the
section structure of a real arrangement — skip with a `SKIPPED:` line saying
so. Everything else runs anywhere. On a checkout that is supposed to have the
samples, `PROJECTMP4_REQUIRE_SAMPLES=1` turns those skips back into failures,
because an unread skip is indistinguishable from a pass.

### Running the full gate

`cargo test` alone is **not** the whole suite, and on a fresh clone it needs
one environment variable. Three commands:

```
PROJECTMP4_NO_PARITY_ORACLE=1 cargo test --no-fail-fast
```

```
cargo test --test packs --test rank -- --ignored
```

```
PROJECTMP4_REQUIRE_SAMPLES=1 cargo test --no-fail-fast
```

The first is what a clone can run. `PROJECTMP4_NO_PARITY_ORACLE=1` opts out
of the stem- and section-parity checks, which compare against a cache under
`~/Library/Application Support/agent-deck` that exists only on the
maintainer's machine and cannot be regenerated. Those checks **fail rather
than skip** by default, on purpose: they are the only evidence that this
project's stems and section boundaries match the implementation they were
ported from, so they must never pass quietly when they did not run.

The second covers the community-ranked preset manifest and the per-genre
selection profiles: eight tests marked `#[ignore]` because they clone a
preset-pack repository over the network. They are the only coverage the
preset-selection pipeline has, including the only check that
`presets/profiles.toml` has not gone stale against the ranked manifest.

The third is the maintainer's: no parity opt-out, and
`PROJECTMP4_REQUIRE_SAMPLES=1` so that a missing sample is a failure instead
of a skip. That variable matters more than it looks, because `cargo test`
captures a *passing* test's output and shows it only for failures — so the
`SKIPPED:` lines are invisible in a normal run, and on a machine where the
samples are supposed to exist a quietly broken checkout would read as 285
green tests. Where you do want to see them, add `-- --nocapture`.

Use `--no-fail-fast` throughout: plain `cargo test` stops at the first failing
test binary and hides the rest.
