# Vendored crates

These are copies, not dependencies. projectmp4 is one standalone product: it builds and runs on a
machine that has never seen agent-deck, and nothing here resolves outside this repository.
Verify that with `cargo metadata` — every path package must sit under
`/Users/tjw/Developer/projectmp4/`.

| crate | language | size | role |
|---|---|---|---|
| `media-codec` | Rust, 245 lines | 40K | audio decode (symphonia + libopus) and EBU R128 loudness |
| `track-analysis` | Rust, 1063 lines | 10M with models | beats, downbeats, tempo, chroma; owns `build.rs` |
| `loop-analysis` | C++, 229 lines | 36K | scored loop-point ranking kernel |

## Where they came from

Copied from `/Users/tjw/Developer/agent-deck/native/` — and this part matters more than it looks:

**All three directories are untracked in agent-deck.** They appear as `??` in its `git status` and
are in no commit of its history. agent-deck's HEAD at copy time was `4557dc2`
(2026-10-19T12:50:07-04:00), but that commit does not contain this code, and neither does any
other. There is no upstream revision to cite and nothing to diff against.

So once this directory is committed here, **projectmp4 holds the only version-control record of
this code that exists anywhere.** That is a reason to commit it rather than gitignore it, and a
reason to treat edits here as edits to a sole original rather than to a copy.

The copy was verified faithful at the time it was made:

- `diff -r --exclude=target --exclude=*.onnx` against agent-deck's working tree: identical for all
  three directories
- `beat_this_small.onnx` sha256 begins `a5f8d39d989f3185`, `mel_spectrogram.onnx` begins
  `fdd59e65c5153313` — both matching

Copied with `rsync -a --exclude 'target/'`. The directory layout under `crates/` deliberately
mirrors agent-deck's `native/`, which is why nothing inside these crates needed editing:

- `track-analysis/Cargo.toml` keeps `deck-media-codec = { path = "../media-codec" }`
- `track-analysis/build.rs` keeps `.file("../loop-analysis/src/deck_loops.cpp")`

Both now resolve to the copies beside them. Preserve that layout — flattening it would mean
editing vendored source, which makes future diffs against upstream harder than they need to be.

## What was changed outside these directories

Two edits, both in projectmp4 itself:

1. `Cargo.toml` — the two path dependencies point at `crates/track-analysis` and
   `crates/media-codec` instead of absolute paths into agent-deck.
2. `src/audio/analysis.rs` — `DEFAULT_MODELS` resolves to
   `concat!(env!("CARGO_MANIFEST_DIR"), "/crates/track-analysis/models")` instead of an absolute
   agent-deck path.

Nothing inside `crates/` was modified. If that stops being true, record each change here with
its reason, because an unrecorded local edit is the thing that makes a future upstream diff
untrustworthy.

## Models

`track-analysis/models/` is committed, 10 MB:

| file | bytes | purpose |
|---|---|---|
| `beat_this_small.onnx` | 10555592 | beat/downbeat inference (beat-this 1.1.0, small1) |
| `mel_spectrogram.onnx` | 270742 | its mel front end |

Committed rather than fetched on first use, deliberately: this is an offline tool and a render
should never fail because a download did.

## External dependencies

All from crates.io, all pinned. No system libraries, no CMake. `cc` builds the 229 lines of C++.

`beat-this =1.1.0`, `rustfft =6.4.1`, `sha2 =0.10.9`, `symphonia =0.6.1`
(isomp4/ogg/pcm/wav), `symphonia-adapter-libopus =0.3.0`, `ebur128 =0.1.10`, `serde_json`.

## Known loose end

`src/audio/analysis.rs` still probes `~/Library/Application Support/agent-deck` at **runtime**
for a cached analysis, falling back to computing one. That is opportunistic reuse, not a
dependency, and it does not appear in the build graph — but on any machine without agent-deck it
is a wasted lookup on every call. It is unrelated to the build being standalone, which it now is.

## Upstream

agent-deck is where this code was written, not a remote we track, and it never committed it.
Nothing here is pushed back, and `/Users/tjw/Developer/agent-deck/` is read-only from this
repository.

There is therefore no "upstream fix" to pull in the normal sense. If that working tree changes and
the change looks worth having, diff it against this directory and apply it deliberately — but
remember the comparison runs against *its* uncommitted working state, which can change under you
and has no history of its own. This copy is the stable one.
