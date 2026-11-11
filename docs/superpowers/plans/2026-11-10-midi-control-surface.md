# MIDI Control Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Drive the preview from a Numark MixTrack Go (or any class-compliant MIDI controller), record what was performed, and let `render` replay it exactly.

**Architecture:** A device-agnostic verb set (`Action`) that both the keyboard and a MIDI profile produce. MIDI arrives on CoreMIDI's own thread into a queue drained once per frame. Every verb that changes what you see is appended to a performance file; `render --performance` replays it on the offline virtual clock, so the preview stays the render.

**Tech Stack:** `midir =0.11.1` (CoreMIDI backend, virtual ports for hardware-free tests), existing `serde_json`, hand-rolled flat-TOML parser in the style of `src/presets/manifest.rs`.

**Spec:** `docs/superpowers/specs/2026-11-03-projectmp4-design.md` — this plan extends §preview. No spec section covers control surfaces; the decisions below are new and marked as mine.

---

## The device, as verified

Numark MixTrack Go, from the official guide's own callout diagram (`cdn.inmusicbrands.com/Numark/MixTrack Go - User Guide - v1.0.pdf`):

| Per deck (×2) | Shared |
| --- | --- |
| Platter/jog (capacitive top + outer ring) | Browse encoder + push |
| Pitch fader | Load 1 / Load 2 |
| Sync, Cue, Play/Pause | Crossfader |
| 4 performance pads | Main level, Cue level |
| Mode/Shift + 4 mode LEDs | Fade FX (one, not per deck) |
| Acapella, Instrumental | Bluetooth indicator |
| Filter/Low knob, Channel fader, PFL/Cue | |

Retailer sources say 4 pads per deck and the community map's note range (`0x14`–`0x17`) confirms exactly four. Secondary sources claimed one jog / one pitch / one Acapella; the official diagram numbers each of those twice, so **the diagram wins** and those sources were deduplicated control-*type* lists.

**Protocol: plain class-compliant USB MIDI.** No SDK, no driver. Numark's own FAQ lists the driver as "None / Class-compliant". Bluetooth MIDI needs no code either — pair it in Audio MIDI Setup → MIDI Studio and it appears as an ordinary CoreMIDI source.

**There is no official MIDI implementation chart.** No Numark chart, no VirtualDJ XML, no Serato map. The numbers below come from the community Mixxx mapping (`josefveiga/Numark-Mixtrack-Go-mapping-for-Mixxx`, open as mixxx PR #16813), which I downloaded and parsed myself — 72 control bindings, cross-checked against the Mixtrack Pro FX and Platinum FX maps, which share the Go's conventions exactly (deck N = channel N, pads on ch 5–6, browse/load on ch 16, jog CC `0x06`, 14-bit pitch `0x09`/`0x29`, crossfader `0x08`).

Verified numbers (1-indexed channels):

| Control | Status | Number | Notes |
| --- | --- | --- | --- |
| Play / shift | `0x90`/`0x91` | `0x00` / `0x04` | |
| Cue / shift | `0x90`/`0x91` | `0x01` / `0x05` | |
| Sync / shift | `0x90`/`0x91` | `0x02` / `0x03` | |
| Acapella / shift | `0x90`/`0x91` | `0x46` / `0x47` | |
| Instrumental / shift | `0x90`/`0x91` | `0x48` / `0x49` | |
| PFL | `0x90`/`0x91` | `0x1B` | |
| Jog touch on / off | `0x90`/`0x91`, `0x80`/`0x81` | `0x50` | separate from rotation |
| Jog rotate | `0xB0`/`0xB1` | `0x06` | relative; 242 ticks/rev |
| Pitch fader | `0xB0`/`0xB1` | `0x09` MSB + `0x29` LSB | 14-bit, `(msb << 7) \| lsb` |
| Channel fader | `0xB0`/`0xB1` | `0x16` | absolute 0–127 |
| Filter/Low | `0xB0`/`0xB1` | `0x1A` | absolute 0–127 |
| Pads 1–4 / shift | `0x94`/`0x95` | `0x14`–`0x17` / `0x1C`–`0x1F` | note-off on `0x84`/`0x85` |
| Mode | `0x94`/`0x95` | `0x00` | |
| Browse rotate / push | `0xBF` / `0x9F` | `0x00` / `0x07` | relative / button |
| Load 1 / 2 | `0x9F` | `0x02` / `0x03` | |
| Fade FX / shift | `0x9F` | `0x46` / `0x47` | |
| Crossfader | `0xBF` | `0x08` | absolute 0–127 |
| Main / Cue level | `0xBF` | `0x0A` / `0x0C` | absolute 0–127 |

**The hardware tracks Shift itself.** Holding MODE/SHIFT makes each button emit a *different note number*, so we never track modifier state — the shift layer is free, and it doubles the button count.

**LED feedback exists** and is undocumented upstream: send `[status, number, velocity]` back to the same address, velocity `0x00` off, `0x01` dim, `0x7F` bright. `[0x9E, 0x7F, v]` addresses all LEDs. The mapping's author also noted `[0xB0, 0x1F, v]` lights the mode LEDs and Play cumulatively for `v` in 0–5.

### Unverified until the hardware arrives

1. **Jog rotation encoding.** The Go's script only ever tests `value == 127` (counter-clockwise) versus `0x01` (clockwise), implying it emits ±1 with no magnitude — but the rest of the family uses 7-bit two's complement with magnitude, and the family disagrees with *itself* on whether `0x40` means +64 or −64. Treat it as ±1 and calibrate.
2. **Whether buttons send note-off.** The map binds note-off for Play/Cue/Sync/pads but not for Acapella/Instrumental/FadeFX/PFL.
3. **The LED velocity ladder** and whether BLE-MIDI uses the same numbers as USB.

All three are profile data, not code. The calibration checklist at the end closes them in about five minutes.

---

## What projectM actually lets us control

This is the ceiling, read off `src/ffi.rs`. It is smaller than the device suggests, and pretending otherwise would waste the build:

| Exposed | Control value |
| --- | --- |
| `load_preset_file(path, smooth)` | which preset, and hard cut vs soft transition |
| `set_beat_sensitivity(f32)` | **continuous, musical** |
| `pcm_add_float(samples)` | **we choose the samples** — gain, and which stems |
| `set_frame_time(secs)` | we own the clock → pause and scrub |
| `set_fps(i32)` | motion rate (desyncs audio; out of scope) |
| `set_mesh_size(w, h)` | warp detail (reallocates; out of scope) |

**There is no preset blend parameter.** libprojectM 4.2 can cut or cross-*transition* on a timer; it cannot hold a blend at 0.37. So the crossfader cannot mix two presets without two `Instance`s, two FBOs and a composite shader — that is Option 3 below, and it is the only part needing new FFI.

**But the crossfader has a better job already available.** We separate stems with htdemucs and cache them (`src/audio/stems.rs` → `bass`, `drums`, `other`, `vocals`). Feeding projectM a *blend of stems* is a continuous, musical control over what the visualiser reacts to, costs one mix in the PCM step, and needs no new FFI:

```
crossfader 0.0 ──────────────────────────► 1.0
   bass + drums          (rhythm)      other + vocals      (melody)

Acapella    = vocals only          (hard snap to the right)
Instrumental = bass+drums+other     (hard snap away from vocals)
```

That is why this device is a good fit rather than an arbitrary one: its two most distinctive buttons are named for the exact axis we can already compute.

---

## Scope: the decision to make

| | Option 1 — bind only | **Option 2 — bind, record, replay** | Option 3 — + A/B preset blend |
| --- | --- | --- | --- |
| Controller drives the preview | yes | yes | yes |
| New continuous controls | no | beat sensitivity, drive, stem blend | same |
| The mp4 captures your performance | **no** | yes | yes |
| `preview == render` invariant | kept (nothing recorded) | kept (replayed) | kept |
| Needs new FFI | no | no | **yes** |
| Per-frame GL cost | 6.5 ms (unchanged) | ~6.6 ms | ~8.7 ms of 16.7 |

**Recommended: Option 2.** Option 1 makes an expensive toy — you perform, and the render ignores it. Option 2 makes the controller the *authoring* tool for the thing this product exists to produce, and it does so without touching the FFI or the frame budget. Option 3 is a clean follow-on once Option 2 is proven; the crossfader is not idle in the meantime because it has the stem axis.

**This plan implements Option 2.** Task 8 sketches Option 3 as explicitly deferred.

---

## Global Constraints

- `src/cli.rs` documents **"the preview of a track is the render of it"**. Every verb that changes what is on screen must be recordable and replayable, or it must not exist. A control that only the preview can honour is a bug.
- **New flash hazard, new guard.** A pad that cuts presets can be mashed into a strobe. Preset cuts from *any* control are rate-limited to the plan's own `min_gap`; the recorder refuses to record, and the renderer refuses to replay, a cut sequence faster than that floor. One pad toggles `--flash-guard` live and its LED shows the state. This is a new control's own hazard, not a change to the settled off-by-default.
- **Do not grow the frame budget.** Measured baseline (commit `368b3e8`): render 2.2 ms + swap 4.3 ms + pcm 0.0 ms ≈ 6.5 ms draw average against 16.7 ms, 1–3% dropped frames from event-loop wake latency. MIDI is drained from a queue at the top of `draw`, never polled; stem mixing is ≤ 4 × ~800 mul-adds per frame. Task 7 re-measures and the drop rate must not rise.
- Errors use `crate::ffi::{Error, Result}` and `Error::msg(...)`. No `anyhow`.
- No new build tooling. `midir` is the only new dependency, pinned `=0.11.1`.
- MIDI must be opt-in: without `--midi`, no port is opened and no behaviour changes.
- A missing, busy or disconnected controller is a warning on stderr and a preview that keeps working, never an exit.
- `samples/` stays out of commits; so do `.mp4` and recorded `.performance.json` files from real tracks.

## Review Focus

Input classes the spec implies but no task's own tests exercise:

1. **Controller unplugged mid-preview.** CoreMIDI drops the connection; the callback stops. The preview must keep playing to the end. *Test in Task 2.*
2. **A flood of jog ticks** (242/rev, spun hard) between two frames. The queue must not grow without bound and the drain must not blow the frame budget. *Bounded queue + drop-oldest, tested in Task 2.*
3. **A profile that binds one address twice**, or binds a 14-bit LSB with no MSB. Must be refused at load with the offending line, not silently half-applied. *Test in Task 3.*
4. **Stem verbs with no stems separated.** `--clip full` does not separate. The verb must warn once and go inert, never panic or block the frame loop on a multi-minute separation. *Test in Task 5.*
5. **A performance replayed against a different plan** (different profile, fps, or clip). The frame indices mean nothing; it must be refused by plan hash, not replayed wrong. *Test in Task 6.*

---

## File Structure

| File | Responsibility |
| --- | --- |
| `src/midi/mod.rs` | `MidiIn`: open a port by name substring, bounded queue, `drain()`. Owns the `midir` connection and the LED sender. |
| `src/midi/profile.rs` | `Profile`: parse the flat-TOML binding table; `resolve(status, d1, d2) -> Option<Action>`; 14-bit pairing; relative-encoder decode. |
| `src/midi/learn.rs` | `midi-learn`: print decoded incoming messages; `--write` captures a press per named control and emits a profile. |
| `src/control.rs` | `Action` (the verb set) and `Surface` (applies a verb to `PreviewState` + runtime params). Both `key_action` and `Profile::resolve` produce `Action`. |
| `src/performance.rs` | `Performance`: `(frame, Action)` log, JSON load/save, plan hash binding, cut-rate floor enforcement. |
| `profiles/midi/numark-mixtrack-go.toml` | The committed default profile, from the table above. |
| `src/preview.rs` | *modify*: drain MIDI at the top of `draw`, apply verbs, record them, honour transport/stem/drive state. |
| `src/render/loop.rs` | *modify*: replay a performance on the virtual clock. |
| `src/cli.rs` | *modify*: `--midi`, `--midi-profile`, `--record-performance`; `render --performance`; `midi-learn` subcommand. |

---

### Task 1: The verb set

Generalise the existing `KeyAction` into one `Action` both input paths produce. Nothing MIDI yet — this is the seam everything else hangs on.

**Files:**
- Create: `src/control.rs`
- Modify: `src/preview.rs` (`KeyAction` → re-export `Action`; `key_action` returns it; `App::key` matches it)
- Test: `tests/control.rs`

**Interfaces:**
- Produces: `Action` (below), `Surface::apply(&mut self, Action) -> Applied`, where `Applied` says whether the verb changed what is on screen (and so must be recorded).

```rust
pub enum Action {
    Quit,
    Load(Load),                 // existing
    Print(PathBuf),
    Lock(bool),
    StepPreset { forward: bool },
    JumpPreset { slot: u8 },     // pads: index into the pool
    Resync,                      // abandon manual stepping, rejoin the schedule
    TransportToggle,
    SeekBy { secs: f64 },
    SeekTo { secs: f64 },
    CutStyle { smooth: bool },
    StemBlend(f32),              // 0.0 rhythm .. 1.0 melody
    StemSnap(StemSource),        // Acapella / Instrumental / All
    Drive(f32),                  // PCM gain into projectM, 0.0..=2.0
    BeatSensitivity(f32),        // 0.0..=5.0
    FlashGuard(bool),
    Ignored,
}
```

- [ ] **Step 1: Write the failing tests** in `tests/control.rs`

```rust
#[test] fn every_key_still_maps_to_the_same_verb() { /* Esc→Quit, →/←→StepPreset, L→Lock, P→Print */ }
#[test] fn continuous_verbs_clamp_to_their_range() { /* Drive(9.0)→2.0, BeatSensitivity(-1.0)→0.0, StemBlend(1.4)→1.0 */ }
#[test] fn only_verbs_that_change_the_frame_are_recordable() { /* Print and Ignored are not; Lock and Drive are */ }
```

- [ ] **Step 2: Run them and watch them fail** — `cargo test --test control`, expected: `Action` not found.
- [ ] **Step 3: Implement `Action`, `StemSource`, `Applied` and `Surface::apply` in `src/control.rs`**; clamp in the constructors so an out-of-range value cannot exist.
- [ ] **Step 4: Rewrite `key_action` to return `Action`** and re-point `App::key`. Keep every existing key's behaviour byte-identical.
- [ ] **Step 5: `cargo test` — the whole suite, not just the new file.** `tests/preview.rs` exercises `key_action`; it must pass unchanged or the rename lost behaviour.
- [ ] **Step 6: Commit** — `refactor: one verb set for keyboard and controller`

---

### Task 2: MIDI input

**Files:**
- Create: `src/midi/mod.rs`
- Modify: `Cargo.toml` (`midir = "=0.11.1"`), `src/lib.rs`
- Test: `tests/midi_port.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `MidiIn::open(name_contains: Option<&str>) -> Result<MidiIn>`, `MidiIn::drain(&mut self, f: impl FnMut(&[u8]))`, `MidiIn::led(&mut self, status: u8, number: u8, velocity: u8)`, `MidiIn::ports() -> Vec<String>`.

The queue is a `Mutex<VecDeque<[u8; 3]>>` with a hard cap of 512; past the cap the **oldest** is dropped, because a stale jog tick is worthless and a late button press is worse than none. Only 3-byte channel-voice messages are queued; SysEx and clock are discarded in the callback.

Note: a virtual-port round trip was proven to work headlessly on this machine before this plan was written, so these tests need no controller.

- [ ] **Step 1: Write the failing tests** in `tests/midi_port.rs`

```rust
#[test] fn a_virtual_port_round_trips_three_byte_messages() {
    // create_virtual input, connect our own MidiOutput to it, send
    // [0x90,0x00,0x7F] and [0xB0,0x09,0x40], assert drain() yields both in order.
}
#[test] fn a_full_queue_drops_the_oldest_and_keeps_the_newest() {
    // push 600 distinct messages, assert len == 512 and the last one survived.
}
#[test] fn sysex_and_clock_never_reach_the_queue() { /* 0xF0.., 0xF8 */ }
#[test] fn a_missing_port_name_is_an_error_not_a_panic() { /* open(Some("no-such-device")) */ }
#[test] fn dropping_the_connection_leaves_drain_working_and_empty() {
    // Control for Review Focus 1: the preview must survive an unplug.
}
```

- [ ] **Step 2: Run them, watch them fail.**
- [ ] **Step 3: Implement `MidiIn`** in `src/midi/mod.rs`. `midir::os::unix::VirtualInput` is used only by the tests; the shipped path is `MidiInput::ports()` + `connect`.
- [ ] **Step 4: Run them, watch them pass.**
- [ ] **Step 5: `projectmp4 midi-learn --list`** prints the visible ports. Verify by hand against Audio MIDI Setup.
- [ ] **Step 6: Commit** — `feat: read MIDI from a class-compliant controller`

---

### Task 3: The binding profile

**Files:**
- Create: `src/midi/profile.rs`, `profiles/midi/numark-mixtrack-go.toml`
- Test: `tests/midi_profile.rs`

**Interfaces:**
- Consumes: `Action` from Task 1.
- Produces: `Profile::parse(&str) -> Result<Profile>`, `Profile::load(&Path) -> Result<Profile>`, `Profile::resolve(&mut self, msg: [u8; 3]) -> Option<Action>`.

`resolve` takes `&mut self` because 14-bit pairs and relative encoders carry state. Format, parsed by a flat line-based reader in the style of `src/presets/manifest.rs` (no new serde/toml dependency):

```toml
name = "Numark MixTrack Go"

# <status>:<number> = <verb> [arguments]
0x90:0x00 = transport-toggle
0x90:0x02 = resync
0x90:0x46 = stem-snap vocals
0x90:0x48 = stem-snap instrumental
0x94:0x14 = jump-preset 0
0xB0:0x1A = beat-sensitivity 0.0 5.0      # absolute, scaled into the range
0xB0:0x09 = drive-14 msb 0.0 2.0          # pairs with the 0x29 lsb below
0xB0:0x29 = drive-14 lsb
0xB0:0x06 = step-preset relative          # 0x01 forward, 0x7F back
0xBF:0x08 = stem-blend                    # absolute 0..127 -> 0.0..1.0
0xBF:0x00 = step-preset relative
0x9F:0x46 = cut-style toggle
0x94:0x1C = flash-guard toggle
```

- [ ] **Step 1: Write the failing tests** in `tests/midi_profile.rs`

```rust
#[test] fn the_committed_mixtrack_go_profile_parses() { /* load profiles/midi/numark-mixtrack-go.toml */ }
#[test] fn every_binding_resolves_from_its_own_bytes() {
    // For each line of the committed profile, feed the bytes it names and
    // assert resolve returns a verb (not None). Proves the file is live, not decorative.
}
#[test] fn a_fourteen_bit_pair_resolves_once_with_the_joined_value() {
    // msb 0x40 then lsb 0x00 -> one Drive((0x40<<7)/16383 * 2.0); msb alone yields None.
}
#[test] fn a_relative_encoder_signs_both_directions() { /* 0x01 forward, 0x7F back */ }
#[test] fn an_unbound_address_is_ignored() { /* 0x90:0x7E -> None */ }
#[test] fn a_duplicate_address_is_refused_naming_the_line() { /* Review Focus 3 */ }
#[test] fn an_lsb_without_its_msb_is_refused() { /* Review Focus 3 */ }
#[test] fn an_absolute_range_that_is_inverted_or_empty_is_refused() { /* "5.0 0.0" */ }
```

- [ ] **Step 2: Run them, watch them fail.**
- [ ] **Step 3: Write `profiles/midi/numark-mixtrack-go.toml`** from the verified table above. Every line gets a comment naming the physical control.
- [ ] **Step 4: Implement `Profile`.** Store bindings in a `[[Option<Binding>; 128]; 16]`-shaped lookup or a sorted `Vec` — `resolve` runs per message and must not allocate.
- [ ] **Step 5: Run them, watch them pass.**
- [ ] **Step 6: Commit** — `feat: bind a controller through an editable profile`

---

### Task 4: Learn mode

The tool that turns "no hardware yet" into a five-minute job when the box arrives.

**Files:**
- Create: `src/midi/learn.rs`
- Modify: `src/cli.rs` (`midi-learn` subcommand)
- Test: `tests/midi_learn.rs`

**Interfaces:**
- Consumes: `MidiIn` (Task 2), `Profile` (Task 3).
- Produces: `learn(ports: Option<&str>, write: Option<&Path>, controls: &[&str]) -> Result<()>`; `decode(msg: [u8; 3]) -> String` for the printed line.

`midi-learn` with no `--write` is a monitor: every message prints as `note-on  ch1  0x00  vel 127`. With `--write <path>` it walks a named list of controls, prompts for each ("press Play on deck 1"), records the address, and writes a profile.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn a_message_decodes_to_a_readable_line() { /* [0x94,0x17,0x7F] -> "note-on ch5 0x17 vel 127" */ }
#[test] fn fourteen_bit_neighbours_are_flagged_in_the_monitor() { /* 0x09/0x29 noted as a likely MSB/LSB pair */ }
#[test] fn captured_addresses_round_trip_through_the_profile_parser() {
    // Synthesise a capture, write a profile, parse it back, resolve the same bytes.
    // This is the whole point: what learn writes, the profile must read.
}
```

- [ ] **Step 2: Run them, watch them fail.**
- [ ] **Step 3: Implement `learn` and `decode`.**
- [ ] **Step 4: Run them, watch them pass.**
- [ ] **Step 5: Drive it over a virtual port** — send bytes from a scratch sender and confirm the written profile parses. No hardware.
- [ ] **Step 6: Commit** — `feat: midi-learn, to map a controller without reading a spec sheet`

---

### Task 5: Honour the verbs in the preview

**Files:**
- Modify: `src/preview.rs`
- Test: `tests/preview_surface.rs`

**Interfaces:**
- Consumes: `Action`/`Surface` (1), `MidiIn` (2), `Profile` (3), `Stems` (`src/audio/stems.rs`).
- Produces: `RuntimeParams { drive: f32, beat_sensitivity: f32, stem_blend: f32, stem_snap: StemSource, paused: bool, smooth_cuts: bool, flash_guard: bool }` — the live state a performance replays.

Drain at the top of `draw`, before the PCM step, so a verb takes effect on the frame it arrived for:

```rust
// Bounded, non-blocking, no syscalls: the callback thread already did the I/O.
self.midi_in.drain(|msg| {
    if let Some(a) = self.profile.resolve(msg) { pending.push(a); }
});
for a in pending.drain(..) {
    if let Applied::Changed = self.surface.apply(a.clone()) {
        self.performance.record(due.frame, a);
    }
}
```

Stem mixing happens inside the existing PCM step: the window is mixed from the cached stems by `stem_blend`/`stem_snap`, then scaled by `drive`, then handed to `pcm_add_float`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn stem_verbs_warn_once_and_go_inert_without_stems() { /* Review Focus 4 */ }
#[test] fn the_stem_blend_mixes_rhythm_into_melody_monotonically() {
    // blend 0.0 is bass+drums, 1.0 is other+vocals, 0.5 holds energy from both;
    // assert on summed magnitude per band, not on exact samples.
}
#[test] fn drive_scales_the_pcm_handed_to_projectm() {}
#[test] fn pausing_holds_the_frame_time_and_resuming_does_not_jump() {}
#[test] fn preset_cuts_are_rate_limited_to_the_plans_min_gap() {
    // Flash safety. Mash JumpPreset 20x in 100ms; assert at most one load lands.
}
#[test] fn a_jog_flood_between_two_frames_costs_bounded_time() { /* Review Focus 2 */ }
```

- [ ] **Step 2: Run them, watch them fail.**
- [ ] **Step 3: Add `RuntimeParams`, the drain, the stem mix and the cut-rate floor** to `src/preview.rs`.
- [ ] **Step 4: Run them, watch them pass.**
- [ ] **Step 5: Commit** — `feat: drive the preview from a controller`

---

### Task 6: Record and replay

The task that keeps `preview == render` true.

**Files:**
- Create: `src/performance.rs`
- Modify: `src/render/loop.rs`, `src/render/plan.rs` (plan hash), `src/cli.rs`
- Test: `tests/performance.rs`

**Interfaces:**
- Consumes: `Action` (1), `RenderPlan` (`src/render/plan.rs`).
- Produces: `Performance::record(&mut self, frame: u64, a: Action)`, `Performance::save(&self, &Path) -> Result<()>`, `Performance::load(&Path) -> Result<Performance>`, `Performance::bind(&self, plan: &RenderPlan) -> Result<()>`, `Performance::at(&self, frame: u64) -> &[Action]`.

JSON (`serde_json` is already a dependency, and `--dump-plan` sets the precedent):

```json
{ "version": 1,
  "plan_hash": "sha256:…",
  "events": [ { "frame": 412, "action": { "stem-blend": 0.62 } } ] }
```

`plan_hash` is over the plan fields that make frame indices meaningful — profile, pool, fps, clip span, switch frames. `bind` refuses a mismatch with a message naming what differs.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn events_replay_at_exactly_the_frames_they_were_recorded_at() {}
#[test] fn a_performance_is_refused_against_a_plan_it_was_not_recorded_against() {
    // Review Focus 5: change fps, then profile, then clip; each must be refused
    // and the message must name the field.
}
#[test] fn two_replays_of_one_performance_are_byte_identical() {
    // Determinism is the product. Render 60 frames twice, hash both.
}
#[test] fn a_recorded_cut_sequence_faster_than_the_floor_is_refused_at_load() {
    // Flash safety survives the file round trip, not just the live path.
}
#[test] fn a_performance_with_an_unknown_verb_is_refused_naming_it() {
    // Forward compatibility: a newer file must not half-apply.
}
```

- [ ] **Step 2: Run them, watch them fail.**
- [ ] **Step 3: Implement `Performance` and the plan hash.**
- [ ] **Step 4: Replay in `src/render/loop.rs`** — apply `at(frame)` before rendering each frame, on the virtual clock.
- [ ] **Step 5: Wire the CLI** — `preview --midi [--midi-profile P] [--record-performance F]`, `render --performance F`.
- [ ] **Step 6: Run them, watch them pass.**
- [ ] **Step 7: Commit** — `feat: record a performance and replay it in the render`

---

### Task 7: Prove the frame budget held

**Files:**
- Modify: `README.md`, `src/cli.rs` (help text)
- Test: measurement, recorded in the commit message

- [ ] **Step 1: Run the gate** — `cargo test` and `cargo clippy`. Clippy's content-warning count must still be 39.
- [ ] **Step 2: Measure the preview with `--midi` against the `368b3e8` baseline** — 45 s, same track, three runs. Record render/swap/pcm/draw averages, worst settled draw, and dropped-frame percentage.
- [ ] **Step 3: Measure with a jog spun continuously** for 10 s, as the worst realistic message rate.
- [ ] **Step 4: If the drop rate rose**, that is a finding, not a rounding error — drain fewer messages per frame or shrink the queue cap, and measure again.
- [ ] **Step 5: Document it** — the mapping table in `README.md`, the calibration checklist, and `--midi` in the help.
- [ ] **Step 6: Commit** — `docs: the controller mapping, and what it costs a frame`

---

### Task 8 (deferred — Option 3, not in this plan)

A/B preset blend: a second `Instance`, a second `Offscreen`, a composite shader, and `projectm_*` additions for the second context. Doubles the 2.2 ms render cost to ~4.4 ms, taking the draw average to roughly 8.7 ms of 16.7 — it fits, but it eats the headroom that currently absorbs the 1–3% drops. Worth doing only once Option 2 is proven, and only with a fresh measurement.

---

## Calibration checklist (when the hardware arrives)

1. `projectmp4 midi-learn --list` → confirm the port name; set the `--midi` default substring from it.
2. `projectmp4 midi-learn` → press every control once. Diff the printed addresses against the table above and fix any line that differs.
3. **Jog encoding.** Spin slowly, then fast. If values beyond `0x01`/`0x7F` appear, the jog carries magnitude and `step-preset relative` needs the two's-complement decode instead of ±1.
4. **Note-off.** Hold Acapella, Instrumental, FadeFX, PFL. Note which send note-off, so momentary-versus-toggle is right.
5. **LEDs.** Send `[0x9E, 0x7F, 0x7F]` (all on), then the dim/bright ladder per address, and record what lights.
6. **Bluetooth.** Pair in Audio MIDI Setup, repeat step 2, and confirm the numbers match USB.

---

## Self-review

**Spec coverage.** The design doc has no control-surface section; this plan adds one and the decisions are marked as mine. The one spec invariant at risk — "the preview of a track is the render of it" — is the reason Task 6 exists.

**Steps.** Each code step names a file and a signature; bodies appear only for the drain loop and the file formats, where the shape is not implied by the signature.

**Types.** `Action` is defined once in Task 1 and consumed by Tasks 3, 5 and 6 under that name. `RuntimeParams` is produced in Task 5 and replayed in Task 6. `StemSource` spans Tasks 1 and 5.

**Review Focus.** All five uncovered input classes have a named test in an owning task.

**Proportion.** Seven tasks, each with its own test cycle and commit. The largest single piece of new logic is `Profile::resolve`; everything else is plumbing between existing parts.
