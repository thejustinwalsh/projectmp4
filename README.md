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

Presets are chosen by curation, not by a heuristic. The pool is BLTC 2.01 and
the Winamp stock set -- the two community best-of compilations of the MilkDrop
canon -- and a preset's bass reactivity only orders it against others in the
same tier. Cream of the Crop fills a profile the curations cannot, inside that
genre's categories. Variants of one preset (`nz`, `roam`, `ret`) collapse to one
entry, so forty slots are forty different things to watch.

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

## Controller

**Not reachable from the command line yet.** The verbs, the port, the profile
format and the learn mode are built and tested; nothing is wired into
`src/cli.rs`, so none of the commands below run today. What follows is the
design, kept beside the code that implements it.

A profile ships for the Numark MixTrack Go
(`profiles/midi/numark-mixtrack-go.toml`); any class-compliant controller works
with a profile of its own, and `midi-learn` writes one by watching the device.
The surface it is built for:

```bash
projectmp4 midi-learn --list                        # what the system can see
projectmp4 midi-learn                               # print what a control sends
projectmp4 preview track.mp3 --midi                 # bind the shipped profile
projectmp4 preview track.mp3 --midi --midi-profile mine.toml
```

Nothing will open a MIDI port without `--midi`, and a controller that is
absent, busy or unplugged mid-track is a warning on stderr, never an exit.

### What the controls do

One visualiser, two decks, so deck 2 mirrors deck 1 rather than pretending to
be a second channel. The hardware tracks MODE/SHIFT itself — a held shift makes
each button send a different note — so the shift layer costs nothing to support.

| Control | Does |
| --- | --- |
| Crossfader | Blends what the visualiser listens to: rhythm ↔ melody |
| Acapella / Instrumental | The hard snaps of that same axis; shifted, back to the whole mix |
| Filter/Low 1, 2 | Reaction drive, and projectM's beat sensitivity |
| Pitch fader 1, 2 | The same two, 14-bit, for setting a value rather than sweeping it |
| Jog wheels, Browse | Step through the preset pool |
| Pads | Jump to pool slot 0–11 |
| Play | Pause and resume |
| Cue | Back ten seconds; shifted, forward ten |
| Sync | Rejoin the render's preset schedule |
| Browse push, Play+shift | Freeze the preset on screen |
| Fade FX | Soft transition or hard cut |
| Fade FX +shift | Toggle the flash guard |
| PFL, Load 1 | Print the current preset's path, for `render --preset` |
| Load 2, Sync+shift | Print every live parameter as the flags that reproduce it |

The crossfader is the binding the device is shaped for. The four htdemucs stems
sum back to the original track, so an equal mix is the untouched audio and the
two ends are the rhythm section and the melody — and the device happens to have
buttons named Acapella and Instrumental sitting on the same axis. Stem controls
need separated stems: without them they warn once and go inert, since
separation takes several times the track's length and cannot happen mid-frame.

### What it does not do

**The controller moves the preview, not the render.** There is no performance
recording yet, so a look you find with the knobs does not reach the mp4 by
itself — print the state and pass the flags to `render`. This follows what the
arrow keys already did: the preview says `[off schedule]` in its title bar when
it has stopped showing what the render would, and never diverges silently.

**The crossfader cannot blend two presets.** libprojectM 4.2 can cut or
cross-transition on a timer; it has no blend position to scrub. A real A/B
preset mix needs a second projectM instance and a composite pass, which would
roughly double the 2.2 ms render cost.

Preset changes from any control are rate-limited to the plan's own minimum
switch gap. Pads that cut presets are how you build a strobe by accident, and
the limiter is cheaper than the mistake.

### If the numbers are wrong

There is no official Numark MIDI implementation chart for the MixTrack Go. The
shipped profile's addresses come from the community Mixxx mapping
(`josefveiga/Numark-Mixtrack-Go-mapping-for-Mixxx`, upstream PR #16813),
cross-checked against the Mixtrack Pro FX and Platinum FX maps, which share its
conventions: deck N on channel N, pads on channels 5–6, browse and load on
channel 16, jog rotation on CC `0x06`, 14-bit pitch on CC `0x09` + `0x29`.

Two things only hardware settles. The jog may send ±1 per tick or a signed
magnitude — the family disagrees with itself on whether `0x40` means +64 or
−64. And not every button is known to send note-off. Run `midi-learn`, compare,
and edit the profile; the plan at
`docs/superpowers/plans/2026-11-10-midi-control-surface.md` has the checklist.

Bluetooth needs no support code: pair the controller in Audio MIDI Setup and it
appears as an ordinary CoreMIDI source.

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

The second covers the curated preset manifest and the per-genre
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
