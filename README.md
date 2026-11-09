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
PROJECTMP4_NO_PARITY_ORACLE=1 cargo test --no-fail-fast
```

There is no setup step. The environment variable is explained under [Running
the full gate](#running-the-full-gate); a clone needs it, and nothing else.

### What is committed, and what is fetched

Everything needed to build and run is in the repository. What is left out is
model weights — bytes nobody edits, published elsewhere, and pinned here by an
exact commit or revision plus a sha256. Each one is fetched by whichever stage
first needs it, so nothing has to be run by hand:

| weight | size | fetched by | needed for |
| --- | --- | --- | --- |
| `beat_this_small.onnx`, `mel_spectrogram.onnx` | 11 MB | `build.rs`, when absent | every render: the beat grid |
| `htdemucs-q8_0.gguf` | 62 MB | first `--clip drop` / `--clip intro` | separating stems to find a section |
| community preset packs | 200 MB | `cargo run --release -- init` | the preset lists in `presets/profiles.toml` |

`scripts/fetch-models.sh` is what does the first two, and it takes a group
(`beat`, `stems`, or neither for both) so each stage asks only for what it
needs. Running it yourself just does that work earlier. It is idempotent: a
file already present with the right hash is left alone, and a hash that does
not match the pin is refused rather than used.

The rest of `vendor/stems-runtime` **is** committed — a prebuilt arm64 worker
and two dylibs, 5.4 MB. Unlike the weights these are published nowhere, so
there is no pin that would make fetching them reproducible, and a copy in the
repository is the only version-control record of them that exists. They are
relocatable (`@loader_path`, and otherwise only system libraries), so they run
from wherever the checkout is.

## Output

1080p60 h264 in mp4, AAC at 256 kb/s, `+faststart`.

The default encode is **capped CRF**: CRF 20 under a hard 12 Mb/s ceiling, so a
3:39 track comes out around 350 MB and the size follows from the duration
rather than from the content.

The ceiling is not a preference, it is the only thing that works. A full-frame
MilkDrop preset is high-entropy in every tile of every frame and gives CRF no
cheap scenes to save on, so quality alone cannot size it. Measured on 20 s at
1080p60:

| setting | bitrate | a 3:39 track would be |
| --- | --- | --- |
| `-crf 17`, uncapped | 67.8 Mb/s | 1.86 GB |
| `-crf 20`, uncapped | 48.4 Mb/s | 1.33 GB |
| `-crf 20` under 12 Mb/s | 12.7 Mb/s | 348 MB |

Reaching 12 Mb/s on the CRF knob alone would take about CRF 32, which looks
like CRF 32. Capping instead costs SSIM 0.879 against the uncapped CRF 17
reference and leaves the frame-to-frame luminance range unchanged (12.9–209.6
against 13.0–209.5), so the motion survives; 12 Mb/s is also what YouTube asks
for at 1080p60. `--fast` (VideoToolbox) has no CRF, so it states the same
ceiling as an average bitrate.

`--target discord|x|youtube` sizes the output for where it is going. Because
the default cannot exceed the ceiling, a target only has to intervene when the
cap is tighter than that — for Discord's 500 MB, past about 241 s — which is
why a 3:39 track needs no target to fit.

## Testing

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
