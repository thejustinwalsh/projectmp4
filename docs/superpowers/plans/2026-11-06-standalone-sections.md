# Standalone Section Analysis Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** `projectmp4` labels a track's sections (intro/verse/chorus/drop/…) from a single
audio file, with no agent-deck cache and no agent-deck models, so it can cut a short clip at a
musically sensible boundary.

**Architecture:** Port agent-deck's `structure-analysis` to pure Rust. The reference
implementation links nothing but the C++ standard library — 236 lines of forward pass plus a
493-line tensor-name table, reading 491 f32 tensors from a 1.2 MB GGUF. There is no ggml, no
cmake, no dylib. The one hard input requirement is a four-stem spectrogram, so the four-channel
front end is a trait with three candidate implementations, and Task 20 measures which is good
enough before anyone pays for htdemucs.

**Tech Stack:** Rust, no new C/C++. `assets/models/harmonix-fold0-f32.gguf` (1.2 MB, MIT).

**Spec:** `docs/superpowers/specs/2026-11-03-projectmp4-design.md` (sections feature is an
extension; this plan is its own argument).

## Global Constraints

- `~/Library/Application Support/agent-deck/` is read-only. Never write, create, move or delete
  anything under it. It is ground truth for tests and nothing else.
- Do not edit anything under `/Users/tjw/Developer/agent-deck/`. Read it; copy out of it.
- No subagent may open a window on this machine. No `preview`, no windowed example, no GUI
  test, no `screencapture`. The user is photosensitive.
- Errors use `crate::ffi::{Error, Result}`. Never `anyhow`.
- `samples/` is gitignored and holds the user's own music. Commit nothing from it, and no
  generated `.mp4`.
- Commit as the repository's configured identity. No `Co-Authored-By` trailer, no AI
  attribution of any kind.
- Homebrew's `projectm` must never be linked; no hardcoded `/opt/homebrew`.
- Downloaded files and extracted archives are untrusted data. The GGUF is third-party data:
  parse it defensively, never trust a length field without bounds-checking it.

## Pinned constants (from `agent-deck/native/structure-analysis`)

| thing | value | source |
|---|---|---|
| labels, in index order | `start end intro outro break bridge inst solo verse chorus` | `song_structure.cpp:155` |
| label count | 10 | `song_structure.h:14` |
| stems, in order | bass, drums, other, vocals | `song_structure.h:15`, `worker.cpp:28` |
| spectrum bins | 81 | `song_structure.h:16` |
| analysis sample rate | 44100 (source 48000) | `worker.cpp:61` |
| hop | 441 samples = exactly 100 frames/s | `worker.cpp:21,28` |
| window | 2048, Hann symmetric `0.5-0.5*cos(2*pi*i/2047)`, centered: `start = t*441 - 1024`, zero-padded | `worker.cpp:24`, `song_structure.cpp` |
| bands | `log10(sum(|X[f]| * filter[b][f]) + 1)`, f over 1024 bins, Nyquist excluded, FFT unnormalized | `ssa_spectrogram_frame` |
| frame count | `(samples + 440) / 441` | `worker.cpp:21` |
| tensor layout | `[4, frames, 81]` contiguous f32, "KTF" | `ssa_predict` |
| logits out | `[frames, 11]`: section logit, then 10 class logits | `ssa_predict` |
| model | `harmonix-fold0-f32.gguf`, 491 tensors, 1246176 bytes, sha256 `f9b48befd3b611d8d11285ad8602ca28cd30457bfdc58940e0bf88ea858cdfe6` | `vendor/song-structure/manifest.json` |
| peak memory | ~1 GB for a full track | `sections/jobs/journal.json` |

`ssa_predict`'s own contract warns that windowed overlap stitching is **not** equivalent to
full-track NATTEN inference. The port runs the whole track in one pass, as the reference does.

Architecture, extracted from the reference and recorded in full at
`.superpowers/sdd/2026-11-03-projectmp4/ssa-architecture.md`:

| thing | value |
|---|---|
| encoder layers | 11, identical in shape; 43 tensors each (473 of the 491; 18 top-level) |
| per layer | `timelayer` then `instlayer` |
| `timelayer` | time-only attention, twice over the same input, dilations `1<<i` and `2<<i` (so 1..1024 and 2..2048), outputs concatenated to 48 wide |
| `instlayer` | 5x5 attention over stem and time jointly, dilation 1, stem axis zero-padded 4 -> 5 |
| frequency attention | none |
| window / heads | window 5; 2 heads of dimension 12; q divided element-wise by `sqrtf(12)` |
| relative position bias | `rpb`, `{2,9}` for `timelayer`, `{2,9,9}` for `instlayer` |
| edges | shifted, clamped windows — **not** zero padding |
| output columns | 0 = section logit; 1..10 = classes in the order above |
| unused tensors | `beat_classifier`, `downbeat_classifier` are in the schema and never read |
| postprocess | NMS half-width 48 (filter 97); peak window 1200; divisor 1200; strength factor 0.5; threshold `strength > 0` |
| `meanClassProbability` | mean over the span's frames of softmax over the **10 class logits only** |
| working bytes | `16 MiB + max(frames, 10240) * 24576`; 1 GiB at ~43,600 frames |

Dilation reaches 2048 at layer 10, which at 100 fps is a ~20 s reach per side per attention.
That is why the reference runs the whole track in one pass and why it needs ~1 GB.

## Ground truth available on disk (read-only)

- `~/Library/Application Support/agent-deck/sections/jobs/artifacts/` — content-addressed
  results carrying `{"result":{"frames":N,"segments":[{"startSeconds","endSeconds","label",
  "meanClassProbability"}]}}` and the `stemBundleHash` that produced them.
- `~/Library/Application Support/agent-deck/stems/bundles/` — 21 htdemucs bundles, the exact
  four-stem audio the cached results were computed from.
- `sections/jobs/journal.json` — maps job ids to plans and stage algorithms.

That pairing is what makes Tasks 18–20 measurable rather than asserted.

## Review Focus

1. A GGUF whose header lengths overflow or point past end of file — must be `Err`, never a
   panic or an out-of-bounds read. It is third-party data.
2. A track shorter than one 2048-sample window, and a track of exactly 0 samples — must give
   either an empty segment list or a clean `Err`, not a divide-by-zero or a 1-frame tensor the
   model rejects.
3. A 10-minute track: 60,000 frames at ~1 GB for 22,000 means allocation must be checked and
   refused with a clear message, not OOM-killed mid-render.
4. Non-finite PCM (a corrupt decode) reaching the spectrogram — the reference throws on
   `!isfinite`; the port must return `Err` with the frame index.
5. A segment list whose boundaries do not cover the track, or that comes back unsorted or
   overlapping — clip selection downstream must not silently pick a negative-length window.

---

### Task 17: GGUF reader and tensor schema

**Files:**
- Create: `src/audio/sections/gguf.rs`, `src/audio/sections/schema.rs`,
  `assets/models/harmonix-fold0-f32.gguf`, `assets/models/README.md`
- Modify: `src/audio/mod.rs`
- Test: `tests/sections_gguf.rs`

**Interfaces:**
- Produces: `pub struct Weights` with `pub fn load(path: &Path) -> Result<Weights>` and
  `pub fn tensor(&self, name: &str) -> Result<(&[f32], &[usize])>`
- Produces: `pub const SCHEMA: &[(&str, &[usize])]` — the 491 expected names and shapes

- [ ] **Step 1: Vendor the model and its provenance**

Copy `/Users/tjw/Developer/agent-deck/examples/ui-wgpu/native/models/harmonix-fold0-f32.gguf`
(or wherever `find` locates it under agent-deck) to `assets/models/`. Verify after copying:

```
f9b48befd3b611d8d11285ad8602ca28cd30457bfdc58940e0bf88ea858cdfe6   1246176 bytes
```

Fail the task if either differs. Write `assets/models/README.md` recording: the upstream model
(`https://huggingface.co/taejunkim/allinone`, revision
`379e5fd010b3fdd0ee8381ff8cbcfa51d70b5c19`, MIT), the MLX weight conversion it derives from
(`https://github.com/ssmall256/all-in-one-mlx`, revision
`8f67ccd0f6460a1e02bc1c68d6d1c3572a16e368`, `mlx-weights/harmonix-fold0_mlx.npz`, sha256
`2d04d162842b048c34d9476c061f2612e4b9cd339d264aa2fb6ec88c59fde1c0`), the GGUF's own sha256 and
byte count, and that the format is `GGUF v3 / F32 / harmonix-f32-v1` with 491 tensors.

Confirm `git check-ignore -v assets/models/harmonix-fold0-f32.gguf` returns nothing before
committing.

- [ ] **Step 2: Transcribe the schema**

Convert `/Users/tjw/Developer/agent-deck/native/structure-analysis/tensor_schema.inc` (493
lines of `{"name", {dims}},`) into `SCHEMA` in `src/audio/sections/schema.rs`. Do this with a
script, not by hand, and assert the entry count afterwards.

- [ ] **Step 3: Write the failing tests**

```rust
#[test]
fn the_vendored_model_matches_the_schema_exactly() {
    let w = Weights::load(Path::new("assets/models/harmonix-fold0-f32.gguf")).unwrap();
    assert_eq!(SCHEMA.len(), 491, "schema transcription lost entries");
    for (name, dims) in SCHEMA {
        let (data, shape) = w.tensor(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(shape, *dims, "{name} shape");
        assert_eq!(data.len(), dims.iter().product::<usize>(), "{name} length");
        assert!(data.iter().all(|v| v.is_finite()), "{name} has non-finite weights");
    }
    assert_eq!(w.len(), 491, "model carries tensors the schema does not name");
}

#[test]
fn a_truncated_or_lying_gguf_is_an_error_not_a_panic() {
    // Three fixtures built in the test from the real file's bytes:
    //   (a) the first 64 bytes only
    //   (b) full header, body truncated to half
    //   (c) a tensor dimension field overwritten with u64::MAX
    // Each must return Err. None may panic, allocate unboundedly, or read out of bounds.
}

#[test]
fn an_absent_tensor_names_itself_in_the_error() {
    // w.tensor("does.not.exist") -> Err whose message contains the name.
}
```

- [ ] **Step 4: Run them and confirm they fail**

Run: `cargo test --test sections_gguf`
Expected: FAIL — `Weights` unresolved.

- [ ] **Step 5: Implement**

A GGUF v3 reader is a magic number, a version, tensor and metadata counts, a metadata KV
block, then a tensor directory of (name, n_dims, dims, type, offset) followed by an aligned
data section. Only `GGML_TYPE_F32` need be supported; any other type is `Err`. Bounds-check
every offset and length against the file size **before** slicing, and reject any count or
dimension that would overflow on multiply. Memory-map or read once; do not re-read per tensor.

- [ ] **Step 6: Run the tests**

Run: `cargo test --test sections_gguf` → PASS. Then `cargo test` → no count goes down.

- [ ] **Step 7: Commit**

```bash
git add assets/models src/audio/sections tests/sections_gguf.rs src/audio/mod.rs
git commit -m "feat: GGUF reader and pinned tensor schema for the harmonix section model"
```

---

### Task 18: The four-stem spectrogram

**Files:**
- Create: `src/audio/sections/spectrogram.rs`
- Test: `tests/sections_spectrogram.rs`
- Test fixture helper: `tests/common/agentdeck.rs`

**Interfaces:**
- Consumes: nothing from Task 17
- Produces: `pub fn resample_48k_to_44k1(input: &[f32]) -> Result<Vec<f32>>`
- Produces: `pub const BINS: usize = 81; pub const HOP: usize = 441; pub const SR: u32 = 44100;`
- Produces: `pub fn frames_for(samples: usize) -> usize`
- Produces: `pub fn frame_bands(samples_2048: &[f32; 2048], out: &mut [f32; 81]) -> Result<()>`
- Produces: `pub fn stack(stems: &[Vec<f32>; 4]) -> Result<(Vec<f32>, usize)>` returning the
  contiguous `[4, frames, 81]` tensor and the frame count

- [ ] **Step 0: Port the resampler first — the parity gate depends on it**

The model analyses at 44100 Hz but the stems are 48000 Hz, and agent-deck resamples with a
specific filter: `runtime.json` records `sourcePolicy: "sinc64-centered-v1/wola6s-hop4.5s-v1"`,
and the implementation is
`/Users/tjw/Developer/agent-deck/native/structure-analysis/src/wav48_stream.h` (91 lines,
read-only). Port it exactly — a 64-tap centred sinc with different windowing or a different
phase convention will shift every frame and fail Step 1 for a reason that looks like a
spectrogram bug. Test it on its own first: a 1 kHz sine resampled 48k -> 44.1k must stay 1 kHz
within a few cents, length must be `ceil(n * 44100 / 48000)` (confirm the exact rule against
the header), and the group delay must match the reference's centring.

Section 10 of the architecture note now specifies it. The values, so the test can be written
before the source is opened:

| thing | value |
|---|---|
| filter | 64-tap Hann-windowed sinc, rational 147/160, polyphase, table precomputed in `double` |
| phase | recomputed from the output index, never accumulated: `center = n*160/147`, `phase = n*160 % 147` |
| tap k | reads source index `center + k - 31`; distance `phase/147 - (k-31)` |
| window | `0.5 + 0.5*cos(pi*d/32)` |
| cutoff | 0.459375 |
| normalisation | each phase normalised to unit sum |
| group delay | zero — output 0 aligns to input 0 |
| edges | source indices clamped at both ends |
| output length | `(src*147 + 159) / 160` |
| stereo | downmixed per tap as `(L+R)*0.5f` in f32; the 64-tap sum accumulates in `double`, then casts to f32 |
| gain / dither | none |

Recomputing the phase from the output index rather than accumulating it is the detail to copy
exactly: an accumulator drifts, and the drift is invisible until the parity gate fails.
Resampled indices outside `[0, (src*147+159)/160)` are **zero** in the STFT frame, not clamped.

- [ ] **Step 1: Write the failing tests**

The first is the one that matters: a numerical parity gate against agent-deck's own output.

```rust
#[test]
fn bands_match_the_reference_implementation_on_real_stems() {
    // Pick one bundle from ~/Library/Application Support/agent-deck/stems/bundles/ whose
    // manifest resolves four stems, decode them to 44.1k mono f32, and run stack().
    // Oracle: run agent-deck's own structure worker over the same stems.
    //   /Users/tjw/Developer/agent-deck/.tools/... (locate the built structure_worker; if no
    //   binary exists, build it in a scratch dir OUTSIDE both repos with cmake, or skip with
    //   #[ignore] and say so in the report — do not weaken the tolerance instead).
    // Compare element-wise.
    assert!(max_abs_diff < 1e-4, "spectrogram diverges from the reference: {max_abs_diff}");
}

#[test]
fn framing_is_centred_and_hop_441_exactly() {
    assert_eq!(frames_for(0), 0);
    assert_eq!(frames_for(1), 1);
    assert_eq!(frames_for(441), 1);
    assert_eq!(frames_for(442), 2);
    assert_eq!(frames_for(44100), 100, "441 hop must give exactly 100 frames per second");
    // A unit impulse at sample 0 must land in frame 0 (window starts at -1024, so the
    // impulse sits at window index 1024): assert bands(frame 0) > bands(frame 3).
}

#[test]
fn a_dc_free_sine_puts_its_energy_in_one_band_region() {
    // 1 kHz sine at 44.1k: the peak band index must be stable and interior (not 0, not 80),
    // and total energy must exceed a silent frame's by a wide margin. Pins the filter bank
    // orientation — a reversed bank would still produce 81 plausible-looking numbers.
}

#[test]
fn non_finite_pcm_is_an_error_naming_the_frame() {
    // A NaN at sample 5000 -> Err mentioning the frame index, not a NaN-filled tensor.
}

#[test]
fn stems_of_different_lengths_are_rejected() {
    // worker.cpp:37 requires equal frame counts; so does this.
}
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test --test sections_spectrogram`
Expected: FAIL — module unresolved.

- [ ] **Step 3: Implement**

A direct port of `ssa_spectrogram_frame` and the framing loop in `worker.cpp:21-27`. Build the
81 filter rows once (`make_filters()` in `song_structure.cpp` — port it exactly; read it, do
not guess the band edges) and reuse them. Use a real FFT crate (`rustfft`) rather than the
reference's naive transform, but keep the reference's normalisation: unnormalized forward FFT,
magnitudes over bins `0..1024`, Nyquist excluded. Stream the windows — never materialise a
full-track complex buffer.

- [ ] **Step 4: Run the tests**

Run: `cargo test --test sections_spectrogram` → PASS, parity test included or explicitly
`#[ignore]`d with the reason in the report.

- [ ] **Step 5: Commit**

```bash
git commit -am "feat: four-stem 81-band spectrogram, parity-checked against the reference"
```

---

### Task 19: The forward pass and post-processing

**Files:**
- Create: `src/audio/sections/model.rs`, `src/audio/sections/postprocess.rs`
- Modify: `src/audio/sections/mod.rs`
- Test: `tests/sections_model.rs`

**Interfaces:**
- Consumes: `Weights` (Task 17), the `[4,frames,81]` tensor (Task 18)
- Produces:
```rust
pub const LABELS: [&str; 10] =
    ["start","end","intro","outro","break","bridge","inst","solo","verse","chorus"];
pub struct Section { pub start: f64, pub end: f64, pub label: u32, pub confidence: f32 }
pub fn predict(w: &Weights, spec: &[f32], frames: usize) -> Result<Vec<f32>>; // [frames, 11]
pub fn postprocess(logits: &[f32], frames: usize) -> Result<Vec<Section>>;
pub fn estimated_working_bytes(frames: usize) -> u64;
```

**Shape convention, settled by Task 17 — read this before anything else.** GGUF stores tensor
dimensions fastest-varying first. Task 17's reader **reverses them**, so every shape
`Weights::tensor` hands you is in row-major numpy order and matches `SCHEMA` exactly as written.
Treat them as row-major numpy shapes throughout the forward pass. Getting this backwards produces
a model that runs, allocates correctly and returns plausible-looking logits that are wrong, which
the parity gate will catch but only after you have written the whole thing — so settle it first by
asserting one known tensor's shape (`embeddings.conv1.weight` is `{24,1,12,12}`) before you build
on it.

- [ ] **Step 1: Read the reference and write the architecture note**

Before writing code, read
`/Users/tjw/Developer/agent-deck/native/structure-analysis/src/song_structure.cpp` in full and
write `.superpowers/sdd/2026-11-03-projectmp4/ssa-architecture.md`: every layer in order, its
tensor names, shapes, activation, and the exact neighbourhood-attention window. A controller
may already have placed that file there — if so, check it against the source rather than
trusting it, and correct it.

- [ ] **Step 2: Write the failing test**

One test decides this task. It is an end-to-end parity gate, and nothing weaker will do:

```rust
#[test]
fn segments_match_agent_decks_cached_result_for_the_same_stems() {
    // For every artifact under sections/jobs/artifacts/ that parses as a result with
    // segments and a stemBundleHash whose bundle is present:
    //   decode the bundle's four stems -> stack() -> predict() -> postprocess()
    // Assert, against the cached segments:
    //   - the same number of segments
    //   - identical labels, in order
    //   - every boundary within 0.02 s (two frames at 100 fps)
    //   - every confidence within 0.01 of meanClassProbability
    // Assert at least one pair was actually compared, so an empty cache cannot pass this.
}

#[test]
fn working_bytes_are_refused_before_they_are_allocated() {
    // estimated_working_bytes(60 * 60 * 100) must be reported, and predict() must return Err
    // naming the figure rather than attempting the allocation, for a frame count whose
    // estimate exceeds a documented ceiling.
}

#[test]
fn degenerate_inputs_are_clean_errors() {
    // frames = 0; spec.len() not equal to 4*frames*81; a spec containing NaN.
}

#[test]
fn segments_tile_the_track_in_order() {
    // On the real result: sorted, non-overlapping, contiguous, first start == 0.0,
    // last end == frames as seconds within one frame.
}
```

- [ ] **Step 3: Run it and confirm it fails**

Run: `cargo test --test sections_model`
Expected: FAIL — `predict` unresolved.

- [ ] **Step 4: Implement**

Port the forward pass from the architecture note. Pure Rust, no new dependencies beyond a
matmul you write yourself or an existing one already in the tree. Single-threaded and
batch-1 like the reference; correctness first, and do not reorder reductions in ways that move
results past the 0.02 s gate. Then port `ssa_postprocess`: upstream peak picking on the section
logit, then mean class probability over each resulting span.

**Reproduce the reference, do not improve it.** The architecture note names four places where
a reimplementation that is "more correct" will fail the gate. Carry them over deliberately and
put a comment on each saying it is deliberate:

1. Window geometry is computed from the **padded** length, not the real frame count.
2. `instlayer`'s zero-padded stem row is a **live key and value**, not masked out. Masking it
   is the obvious fix and it changes every output.
3. The `rpb` edge-index mapping at the first and last windows.
4. The mix of `float` and `double` in the postprocess reductions, which section 7 of the note
   spells out.

If the parity gate fails by a small margin, the cause is one of those four, a reduction order,
or a missing epsilon — not a tolerance that needs loosening. Report a failure rather than
widening the gate. Do **not** start a parity investigation on fused multiply-add. It was considered and ruled out:
FMA moves a result by ~1e-7 relative, the recorded native-versus-MLX logit spread is already
~1e-5, and a peak can only move if two frames' sigmoids sit within ~1e-5 — which the 97-frame
non-maximum suppression and the 1200-frame dominant-peak test make rare. Confidences move by
~1e-6, far under the 0.01 gate. If the gate fails, the cause is one of the four quirks above or
a reduction order. Look there.

`beat_classifier` and `downbeat_classifier` are in the GGUF and never read. Do not wire them
up — projectmp4 already has beats from `deck-track-analysis`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --test sections_model` → PASS. Then `cargo test` → all green.

- [ ] **Step 6: Report the cost**

Time `predict` on a full-length track and report wall-clock seconds and peak RSS. The render
itself takes ~18 minutes, so anything under a minute is free; say the number either way.

- [ ] **Step 7: Commit**

```bash
git commit -am "feat: pure-Rust harmonix section model, parity-checked end to end"
```

---

### Task 20: Four-channel front ends without stems, and the measurement

This is the task that decides whether htdemucs ever gets ported. It produces a number, not an
opinion.

**Files:**
- Create: `src/audio/sections/frontend.rs`
- Create: `examples/section_frontends.rs`
- Test: `tests/sections_frontend.rs`

**Interfaces:**
- Produces:
```rust
pub trait FourChannel { fn channels(&self, mono: &[f32], sr: u32) -> Result<[Vec<f32>; 4]>; }
pub struct MixFourWays;   // the mix in all four slots
pub struct HpssBands;     // harmonic/percussive split, then band-split the harmonic part
pub struct CachedStems;   // reads an agent-deck bundle; test-only ground truth
```

`CachedStems` must be `#[cfg(test)]` or behind a non-default feature, and a release build must
not be able to reach it. The entire point of this plan is that projectmp4 needs no agent-deck
cache; a front end that reads one would quietly put the dependency back, and the feature would
then fail on any machine that has never run agent-deck. Prove it: after implementing, run
`cargo build --release` and confirm with `nm`/`strings` that the binary contains no reference to
`Application Support/agent-deck`, and say so in the report.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn every_frontend_produces_four_equal_length_channels() { /* including a 100-sample input */ }

#[test]
fn hpss_sends_transients_to_the_drum_slot() {
    // A click train plus a sustained sine: the drum slot's energy ratio between the two must
    // exceed the bass/other/vocal slots' by a stated factor. Pins that HPSS is live rather
    // than returning the mix four times under a different name.
}
```

- [ ] **Step 2: Implement both front ends**

`MixFourWays` is trivial. `HpssBands` is median filtering on the magnitude spectrogram —
across time for the harmonic estimate, across frequency for the percussive — then Wiener-style
soft masks; the percussive signal becomes `drums`, and the harmonic signal is split into
`bass` (below ~250 Hz), `other` (250 Hz–4 kHz) and `vocals` (a 200 Hz–4 kHz band emphasising
the centre channel if stereo is available, otherwise the same harmonic mid-band). State in a
doc comment that these are approximations of htdemucs stems, not separations.

- [ ] **Step 3: Measure**

`examples/section_frontends.rs` runs, for every cached result whose stem bundle is present:

| front end | boundary F @0.5s | boundary F @3s | label accuracy (frame-weighted) | chosen-clip agreement |
|---|---|---|---|---|
| CachedStems (htdemucs) | — | — | — | — |
| HpssBands | | | | |
| MixFourWays | | | | |

`CachedStems` is the parity row and must read 1.00 across the board; if it does not, Task 19
regressed and this task stops. Run the measurement through a test or example compiled with the
test/feature configuration, never through the release binary. "Chosen-clip agreement" is whether the highest-confidence
`chorus` span picked from each front end overlaps the one picked from `CachedStems` by at
least half its duration — that, not raw F-measure, is what the feature actually needs.

Write the table to `.superpowers/sdd/2026-11-03-projectmp4/frontend-measurement.md` with the
track count, and name any track where a front end failed outright.

- [ ] **Step 4: Commit**

```bash
git commit -am "feat: stem-free four-channel front ends, with measured accuracy against htdemucs"
```

Report the table in the handback. The controller decides from it whether Task 22 happens; do
not recommend porting htdemucs, and do not recommend against it. Report the numbers.

---

### Task 21: Clip selection and the `--clip` flag

**Files:**
- Modify: `src/cli.rs`, `src/render/plan.rs`, `src/audio/analysis.rs`
- Create: `src/audio/sections/clip.rs`
- Test: `tests/clip.rs`

**Interfaces:**
- Consumes: `Vec<Section>` (Task 19), `Analysis::downbeats` (already present and currently
  unread), `--target` sizing (Task 16)
- Produces: `pub fn choose(sections: &[Section], downbeats: &[f64], want: f64, kind: ClipKind)
  -> Result<(f64, f64)>`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_drop_clip_starts_on_a_downbeat_inside_the_best_chorus() {
    // Synthetic sections + downbeats. The returned start must be a downbeat, must fall within
    // the highest-confidence chorus, and the span must be within one beat of `want`.
}

#[test]
fn a_track_with_no_chorus_falls_back_to_the_loudest_section_and_says_so() {}

#[test]
fn a_want_longer_than_the_track_returns_the_whole_track() {}

#[test]
fn a_want_of_zero_or_a_negative_want_is_an_error() {}

#[test]
fn x_rejects_nothing_we_would_now_produce() {
    // --clip drop --target x must give a span <= 140.0 seconds, the hard X duration cap.
}
```

- [ ] **Step 2: Implement**

`--clip <full|drop|intro>` (default `full`) and `--clip-seconds <f64>`. For `drop`: take the
highest-confidence `chorus` span, else the highest-energy non-`intro`/`outro` span; snap the
start to the nearest downbeat at or after its beginning; extend to the nearest downbeat that
reaches `want`; clamp to the track. Apply a 1.5 s audio fade in and out at the cut, and pass
the span through to the existing render plan so only those frames are rendered. When `--target
x` is set and no `--clip-seconds` is given, default `want` to 135 s — under the 140 s cap with
margin.

- [ ] **Step 3: Run the tests, then render one real clip**

Run: `cargo test --test clip` → PASS, then `cargo test` → all green. Then render
`--clip drop --target x` from a real track headlessly and report: the chosen span, which
section it came from, output duration, file size, and that `ffprobe` reports 1080p60.

- [ ] **Step 4: Commit**

```bash
git commit -am "feat: --clip selects a section-aligned span and renders only that"
```

---

### Task 22 (conditional): vendor htdemucs

**Only if Task 20's measurement says no stem-free front end reaches the bar.** Decided by the
controller from the table, not by an implementer. If it happens it is its own plan: audio.cpp
is a million-line third-party checkout with its own ggml, and the 59 MB `htdemucs-q8_0.gguf`
is too large to commit, so it would be fetched on first use with its pinned sha256
`b0f532ac6e5f373aeb11fa0df73253251e133832d9c8b9942dc58f50bc5b4388`.

---

## Not in this plan

- **Loop points.** `agent-deck/native/loop-analysis` is 229 lines and would give scored
  seamless loop points, but a clip posted to X or Discord wants a fade at a section boundary,
  not an endless loop. Dropped at the user's direction. If an endlessly-looping video is ever
  wanted, that port is small and the chroma/`power_db` features it needs are the only new DSP.
- **Breaking the two agent-deck path dependencies in `Cargo.toml`**
  (`deck-track-analysis`, `deck-media-codec`). Separate concern, separate change: it blocks
  building projectmp4 on any machine without agent-deck checked out at that exact path, and it
  is unrelated to whether sections work from one audio file.
