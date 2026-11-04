# projectmp4 — design

A command-line tool that turns a finished audio track into a shareable 1080p music-visualizer
video, using projectM (the open-source MilkDrop engine) for the visuals.

Status: design, awaiting approval. Nothing built.
Date: 2026-11-03

---

## 1. Intent and the why

Justin produces electronic music — deathstep, dubstep, ambient, dancehall — much of it generated
or assisted with Suno. A finished track is an mp3 and a cover image. To share it on X, Discord or
YouTube he needs a *video*, because those platforms are video-first: an mp3 has nowhere to go.

Today that gap is filled by hand, with a screen recorder pointed at a visualizer, or not filled at
all. The workflow we want is two commands:

```
projectmp4 preview  track.mp3          # does this look good? which preset?
projectmp4 render   track.mp3 --title "..." --art cover.jpg
```

and then a file you drag into a post.

**The why behind the why:** the visuals are not decoration, they are the thing that makes someone
stop scrolling. A static waveform video gets ignored. The bar is "this hits on the kick and looks
deliberate" — which is why preset selection is the heart of this project and not a detail.

### The pen name

Every video carries a **GH0STF1R3** watermark, lower left. This is Justin's pen name for music
and the videos are the main place it will be seen, so the mark has to read at Discord thumbnail
size and not look like a stock font. Designing it is in scope (§7).

---

## 2. Goals

1. **One command, finished file.** `projectmp4 render track.mp3` produces an mp4 ready to upload, with
   sensible defaults and no required flags beyond the inputs.
2. **1080p, 60fps, platform-safe.** H.264 / yuv420p / faststart, AAC audio, correct colour range.
   Plays everywhere without a transcode warning.
3. **Visuals chosen by community curation, not by guesswork.** Preset selection is derived from
   published human curations and from measurable properties of the preset source. It is
   reproducible and auditable, not a hand-wave. (§5)
4. **Genre-aware.** Separate tuned preset profiles for `deathstep`/`dubstep`, `ambient`, and
   `dancehall`, because a drop-driven preset is wrong for ambient and vice versa.
5. **Reacts to the song's structure.** Presets change on musical boundaries — drops, section
   changes — not on a fixed timer. This is the single biggest difference between "a visualizer
   was running" and "this video was made for this track."
6. **Faster than realtime.** A 3-minute track must not take 3 minutes to render. Offline
   rendering is decoupled from playback speed.
7. **Preview before commit.** A realtime preview mode so a bad preset is caught in 10 seconds
   rather than after a full render.
8. **Title, artist and cover art in the upper right. Watermark in the lower left.** Legible at
   thumbnail size, not covering the action.
9. **Deterministic.** Same inputs plus same preset plus same seed produce the same video. Makes
   the thing debuggable and makes A/B comparison of presets meaningful.

## 3. Non-goals

- A GUI. This is a CLI. The preview window is a preview, not an editor.
- Live VJ performance or realtime audio-input visualization. NestDrop and projectMSDL already do that.
- Authoring new `.milk` presets from scratch. We select, rank, and optionally tune existing ones.
- Vertical / square aspect ratios for Reels, Shorts, TikTok. **Deliberately deferred** — the
  layout decisions differ enough to deserve their own pass. Design for it to be addable.
- Lyric video features: synced captions, karaoke timing.
- Batch processing a whole catalogue, or a web service. Single track, local machine.
- Windows and Linux support. macOS / Apple Silicon only, because that is the machine.

## 4. Anti-goals — what would make this a failure even if it ran

- **A video that looks generic.** If the output is indistinguishable from any other "upload your
  mp3, get a visualizer" site, the project failed regardless of code quality.
- **Visuals that ignore the music.** A preset that drifts along the same whether it is a build or
  a drop is the exact thing being fixed. Pretty but unreactive is a failure.
- **A watermark that reads as an afterthought.** Wrong size, wrong opacity, fighting the visuals,
  or looking like default type. The mark is the signature on the work.
- **A curation that is secretly my taste.** Justin asked explicitly for community ranking, not for
  the model's eye. If the shortlist cannot be traced to a human curation or a measurable property
  of the preset file, it does not belong in the shipped manifest.
- **Needing flags to get a good result.** If the defaults produce something mediocre and the good
  output requires knowing six options, it will not get used.
- **A build nobody can reproduce.** If libprojectM has to be coaxed into compiling by hand each
  time, the tool is dead on the next machine.

---

## 5. Preset curation — the core research result

This section records what the research found, because it is the main input to the build and it
took four parallel investigations to establish.

### 5.1 What does not exist

- **There is no community ranking of individual presets.** No tier list, no poll, no "top 100".
  Searched Reddit (blocked in-session), Hydrogenaudio, VirtualDJ, geisswerks.com, and the Winamp
  forum archives.
- **There is no usable rating field.** Old libprojectM read `fRating=` from `.milk` files and was
  removed in commit `5fe7e8c3` (2022-11-17). In Cream of the Crop all 9,795 presets have
  `fRating=5.0` — the curator set it to a constant to run duplicate detection, so it carries zero
  information. In the older packs it is author self-rating, mostly left at the default 3.
- **No preset contest with judged winners.** The 2008 "PRESETS for the CONTEST" thread was an
  informal submission drive, not a ranked competition.

### 5.2 What does exist — the curation ladder

Human curations, strongest signal first:

| Tier | What | Count | Provenance |
|---|---|---|---|
| **BLTC 2.0.1** | bitcore's *"417 of the Best of the Best"*; Flexi proposed it as the pack Winamp should ship | 413 on disk | Winamp forum t=197254, post 2383111 |
| **Winamp 5.5 stock** | *"Geiss and Rovastar put together the presets that shipped with 5.5"* | 557 | forum t=291753, post 2396083 |
| **Geiss favorites** | MilkDrop's author's own picks | zip | geisswerks.com |
| **Cream of the Crop** | 9,795 hand-kept from ~52,000 (19% survived) by Jason Fletcher / "ISOSCELES", over 7 months, judged by VJ performance | 9,795 | projectM default pack since 2022 |
| **ISOSCELES Mashups 2024** | 829 new mashups + 157 "favorites by other authors" | 986 | github.com/Hyphen-05 |

Named individual praise is thin but real, and all of it is recorded with sources in
`docs/research/preset-curation.md` (to be written as part of the build). The strongest superlative
anyone found, for **Nitorami**: *"Your presets are unlike anything I've seen yet, they are a whole
new level of cool."* Authors repeatedly cited for reusable beat-detection and waveform code:
**Krash** (beat detection), **Unchained** (beat code), **Rovastar**, **Zylot**, **fiShbRaiN**,
**shifter**, **Eo.S.**, **Flexi**, **Geiss**.

### 5.3 Cream of the Crop is pre-sorted by feel

The curator organised the pack into 10 categories by "what the preset actually feels like". Measured
counts, with the share using MilkDrop 2 warp/composite shaders:

| Category | Presets | Shader | Use |
|---|---|---|---|
| Reaction | 1791 | 92% | **deathstep / dubstep** — beat-reactive |
| Fractal | 1354 | 63% | |
| Dancer | 1351 | 80% | **dancehall** — rhythmic pocket |
| Waveform | 1279 | 86% | |
| Drawing | 1143 | 97% | |
| Geometric | 1027 | 74% | **deathstep** — hard geometry |
| Sparkle | 797 | 97% | |
| Particles | 389 | 83% | |
| Supernova | 380 | 68% | **drops** — bursts |
| Hypnotic | 280 | **100%** | **ambient** — slow polar warps |

84% of the pack (8,251) uses shaders. Shader presets look substantially better at 1080p than the
MilkDrop 1 per-pixel-equation presets, which is why BLTC — only 7% shader — is used as a *weighting*
signal rather than as the selection pool.

### 5.4 The measurable half: bass-reactivity from preset source

A `.milk` file is plain text. Its `per_frame_*`, `per_pixel_*`, `wave_*`, `shapecode_*`, `warp_*`
and `comp_*` lines are equations that may reference the audio variables `bass`, `bass_att`, `mid`,
`mid_att`, `treb`, `treb_att`, `vol`, `vol_att`. **How hard a preset leans on bass is therefore a
property of the file, readable without rendering anything.**

The scoring used (prototyped in `scratchpad/analysis/`, to be moved into the repo as `projectmp4 rank`):

```
for each .milk:
    extract RHS of all per_frame / per_pixel / per_point / wave / shape / warp / comp lines
    count whole-word references to each audio var
    de-nest: bass count -= bass_att count     (so "bass_att" is not counted twice)
    bass_score  = 3*(bass + bass_att)
    react_score = 3*(bass+bass_att) + 1*(mid+mid_att+treb+treb_att+vol+vol_att)
    is_shader   = has non-empty warp_1= / comp_1= / shader_body
```

Weights are a stated choice, not a discovery: bass is weighted 3× because the target genres are
kick- and sub-driven. This is recorded in the repo so it can be re-tuned and re-run.

Sanity check — top of the measured ranking agrees with the forum reputations, which is the result
that makes the method trustworthy:

```
BLTC, by bass score          Rovastar, fiShbRaiN, Zylot, EoS, Idiot — all forum-cited names
CotC/Geometric top hit       suksma - water cooled red uranium vs dotes   bass=105 react=185
CotC/Supernova top hits      shadowharlequin+flexi, fiShbRaiN, EVET
CotC/Hypnotic top hits       shadowharlequin+flexi, Goody+flexi, suksma, Zylot
```

### 5.5 The shipped selection rule

```
candidate pool  = Cream of the Crop, shader presets only
                  ∩ category whitelist for the genre profile
                  − blacklist (projectM issue #700 known-broken, plus render-verified failures)
rank            = bass_score (genre-weighted)
                  + bonus if also in BLTC            (community best-of)
                  + bonus if also in Winamp stock     (Geiss/Rovastar pick)
                  + bonus if also in Geiss favorites
profile         = top N after ranking
```

Genre profiles:

| Profile | Categories | Weighting |
|---|---|---|
| `deathstep`, `dubstep` | Reaction, Geometric, Supernova | bass-dominant |
| `dancehall` | Reaction, Dancer | mid/bass balanced |
| `ambient` | Hypnotic, Fractal (slow subcats) | low bass weight, favour `react` spread |

The result is committed as a **manifest file** (`presets/profiles.toml`) listing chosen preset
paths with their scores and the tiers they hit. The ranking script is committed next to it so the
manifest can be regenerated and argued with. **A render-verification pass** (render N frames, reject
all-black / shader-error / frozen output) prunes the manifest once — this is a correctness filter
on top of curation, not a taste judgement.

---

## 6. How it gets made — decided

These are settled. Decided by Justin where noted; otherwise my call, marked.

### 6.1 Language and layout — **Justin's call: Rust**

Single binary, FFI straight into libprojectM, no C++/Rust split and no helper-process boundary.

```
projectmp4/
  Cargo.toml
  build.rs                  locate + link libprojectM, bindgen the C API
  src/
    main.rs                 clap CLI: preview | render | rank | init
    ffi.rs                  libprojectM 4.2 C bindings
    gl.rs                   CGL headless context + FBO (render target)
    audio.rs                symphonia: mp3 -> f32 PCM, resample to 44.1k stereo
    analysis.rs             RMS/onset/section detection -> switch points
    render.rs               the frame loop
    overlay.rs              title / artist / art / watermark compositing
    encode.rs               ffmpeg child process, raw frames on stdin
    presets.rs              profiles.toml, selection, blacklist
    rank.rs                 the §5.4 scoring, regenerates the manifest
  presets/profiles.toml     committed ranking output
  assets/gh0stf1r3.svg      the watermark
  docs/research/            sourced curation notes
```

### 6.2 libprojectM — **forced, and the critical constraint**

Must be **4.2.0 from master**, built from source. Two functions that make offline rendering
possible exist only in 4.2.0:

- `projectm_set_frame_time(handle, seconds)` — feed our own clock instead of the wall clock.
  Upstream documents this precisely for *"non-realtime frame rates, e.g. encoding a video as fast
  as projectM can render frames."*
- `projectm_opengl_render_frame_fbo(handle, fbo_id)` — render into our framebuffer.

Homebrew's `projectm` formula is **3.1.12** and has neither (it predates the v4 C API entirely).
The build must vendor or submodule libprojectM and compile it, pinned to a known-good commit.
*My call:* git submodule **pinned to commit `eae22de4`** (2026-10-13, cmake `VERSION 4.2.0`,
186 commits past v4.1.0), built by `build.rs` via cmake.

**Verified, not assumed:** both functions are implemented at that commit, in
`src/libprojectM/ProjectMCWrapper.cpp:245` (`projectm_opengl_render_frame_fbo`) and `:257`
(`projectm_set_frame_time`), declared in `src/api/include/projectM-4/render_opengl.h` and
`parameters.h`. No released tag contains them, so master is required — do not try to pin to a tag.

### 6.3 The render loop

```
             ┌──────────────── analysis pass (once) ────────────────┐
mp3 ──► PCM ─┤ RMS envelope, onset density, section boundaries      │
             │ → preset switch timeline, loudest-bar location       │
             └──────────────────────┬───────────────────────────────┘
                                    ▼
  for frame N in 0..total:
      t = N / fps                                  ← deterministic clock
      if t crosses a switch point:
          projectm_load_preset_file(next, smooth = not a drop)
          └─ hard cut on a drop, soft blend on a section change
      projectm_pcm_add_float(pcm window for [t, t+1/fps))
      projectm_set_frame_time(t)
      projectm_opengl_render_frame_fbo(fbo)
      glReadPixels ──► RGB buffer
      composite overlay (art + title + artist upper-right, watermark lower-left)
      write to ffmpeg stdin
                                    ▼
  ffmpeg: rawvideo stdin + original audio ──► h264 yuv420p + AAC ──► out.mp4
```

Decisions inside that loop (*my call* unless noted):
- **60fps** default, `--fps 30` available. 60 is worth it for strobing drops.
- **Audio window per frame**: feed `samples_per_frame` of PCM each frame so projectM's beat
  detection sees the same audio a realtime player would. `projectm_pcm_get_max_samples()` caps it.
- **Mesh size** raised above default for 1080p; needs empirical tuning, a build-time task.
- **Encoder**: `libx264 -crf 17 -preset slow -pix_fmt yuv420p -movflags +faststart`.
  `h264_videotoolbox` offered as `--fast` for iteration. CRF 17 because gradient-heavy visualizer
  output banding is the main quality risk on these platforms.
- **Audio**: re-encode to AAC 256k rather than copying the mp3 — broadest platform compatibility.
  `--copy-audio` to passthrough.
- **Determinism**: fixed seed, fixed frame clock, no wall-clock reads in the loop.

### 6.4 Overlay layout

```
┌────────────────────────────────────────────────────────┐ 1920×1080
│                                      ┌──────┐          │
│                                      │ art  │  TITLE   │  ← upper right
│                                      │ 180² │  artist  │     art + 2 text lines
│                                      └──────┘          │
│                                                        │
│                     [ visualizer fills the frame ]     │
│                                                        │
│                                                        │
│  ◆ GH0STF1R3                                           │  ← lower left
└────────────────────────────────────────────────────────┘
   64px safe margin on all sides (Discord/X crop safety)
```

- Overlay is composited **in Rust** (`tiny-skia` + `resvg` for the SVG mark, `image` for cover art,
  `cosmic-text` or `fontdue` for type) rather than by ffmpeg filters — keeps layout in code and
  testable, and lets the overlay react to the audio (see below).
- *My call:* overlay elements get a **subtle reactive treatment** — a slight scale or glow on the
  watermark tied to `bass_att`. Sells "made for this track". Must be restrained; `--static-overlay`
  turns it off.
- Legibility over the visuals is the real risk. Soft dark scrim behind the text block, opacity
  tuned during the build.
- Title and artist come from `--title` / `--artist`, falling back to mp3 ID3 tags, falling back to
  the filename.

### 6.5 Commands

| Command | Does |
|---|---|
| `projectmp4 init` | Clone/fetch preset packs + the required texture pack into a cache dir |
| `projectmp4 preview <mp3>` | Realtime SDL window, plays audio, cycles the genre profile; keys to next/prev/lock a preset and print its name |
| `projectmp4 render <mp3>` | The offline render. `--title --artist --art --profile --preset --fps --out` |
| `projectmp4 rank` | Re-run §5.4 scoring over the packs, regenerate `presets/profiles.toml` |

`--profile` defaults to `deathstep`; `--preset <path>` pins one preset and skips switching.

### 6.6 Open — left to the builder

- **Headless GL on macOS 27.** CGL with a pbuffer or a hidden context is the documented approach;
  whether libprojectM 4.2 is happy with a core-profile 4.1 context under this OS version is
  unverified and is the **first thing to spike**, because the whole design rests on it.
  Fallback: a hidden SDL window, which is uglier but known to work.
- Whether `projectm_set_frame_time` alone is sufficient for deterministic beat detection, or
  whether projectM's internal beat detector also needs its own time source fed.
- Exact mesh size, beat sensitivity, and `hard_cut_sensitivity` values — tune empirically.
- Section-detection algorithm. Start with RMS-envelope deltas plus spectral flux; a drop in
  deathstep is a large, sharp low-band energy step and should be easy. Do not reach for a
  heavyweight MIR dependency before trying the simple thing.
- Whether to scrape Geiss's `favorite_presets_2013_12.zip` into the tier bonus. geisswerks.com
  rejects scripted requests, so this needs a browser fetch. Low priority — it is one bonus term.

---

## 7. The GH0STF1R3 mark

Needs designing, in scope, SVG. It will appear at roughly 28px cap height on a 1080p frame, over
moving saturated visuals, which drives every decision: **high contrast, closed silhouette, no fine
detail, readable at 1/8 scale.**

The name is leetspeak — `GH0STF1R3` for GHOSTFIRE — so the zeros and ones are not typos, they are
the identity, and the mark should make them deliberate rather than accidental.

Three directions, to be drawn as SVG and compared at thumbnail size:

1. **Monoline wordmark + glyph.** `GH0STF1R3` in a condensed geometric monoline, letterspaced wide,
   with a small mark to its left — a flame whose negative space reads as a ghost, or vice versa.
   The `0` and the `1`s get drawn as deliberate forms (slashed zero, flag-less one) so the leet
   reads as design. Most legible at small size; safest.
2. **Sigil only.** A single compact glyph — ghost-flame monogram, `G`+`F` interlocked — used alone
   as the watermark, with the full wordmark reserved for larger contexts. Strongest at thumbnail
   size, weakest at communicating the name to someone new.
3. **Stencil / burn.** Wordmark cut as a stencil with breaks in the strokes, as though scorched.
   Most character, most risk of mushing at 28px.

**Decided — direction 1**, with the glyph from direction 2 extracted as a standalone `mark-only`
variant for tight crops. One SVG, two symbols, plus a `--watermark-style word|sigil` flag.

**Colour — Justin's call: cold white only. No accent.** Pure/near-white mark, no second colour.
This is the most versatile choice and it never clashes with a preset, which matters because the
visuals underneath are unpredictable across 9,795 presets. It does mean the mark earns its
distinctiveness entirely from *form* — letterform drawing and the glyph silhouette — rather than
from colour, so the drawing has to be good.

Constraints: pure vector, no raster; single colour; must survive being drawn at opacity ~0.85 over
bright visuals, so it carries a dark halo / soft shadow as a separate optional layer rather than a
glow. Legibility test is a 28px cap-height render over the brightest preset frame in the manifest.

---

## 8. Constraints

- macOS 27.2, Apple Silicon (M2 Pro). No cross-platform burden.
- On the machine already: ffmpeg 7.1.1 (libx264, h264_videotoolbox, hevc, prores), cmake 3.27.7,
  git, python3, node. **Not** present: Rust toolchain (needs installing), libprojectM 4.x.
- Homebrew `projectm` 3.1.12 is installed and is **not usable** — do not link against it.
- Preset packs are large: Cream of the Crop is 9,795 files; the texture pack is required or many
  presets render wrong. These live in a cache dir, not in the repo.
- Preset licensing: Cream of the Crop's LICENSE states most MilkDrop presets carry no explicit
  licence and are "safe to assume" public domain, with a takedown path for authors. Fine for
  personal use; do not redistribute the packs from this repo. Vendor the *manifest*, not the presets.
- Reddit is unreachable from this environment, so no Reddit-sourced citations.

## 9. What done looks like

```
$ projectmp4 render ~/Downloads/suno-track.mp3 --title "Hollow Signal" --artist GH0STF1R3 --art cover.jpg
  analysing      3:42, 44.1kHz stereo — 6 sections, 3 drops
  profile        deathstep (42 presets, 12 switch points)
  rendering      13,320 frames @ 1920x1080 ──────────── 100%   1m48s
  encoding       h264 crf17 + aac 256k
  → suno-track.mp4   48.2 MB
```

Then: it plays in QuickTime, uploads to X and YouTube without a format complaint, the watermark
is readable on a phone, and the visuals visibly hit on the drops.

Verification, before any claim that it works: render a real track end to end, confirm frame count
matches duration × fps, confirm audio and video stay in sync at the end of a 4-minute track, and
look at the output.

---

## 10. Addendum — builder-check resolutions

A fresh engineer read §1–9 and reported what they would still have to guess. Resolutions below.
Items marked **[my call]** are decisions I made so the builder is not blocked; they are recorded
here rather than buried so they can be overruled on sight.

### 10.1 Facts the document was missing

**Preset pack sources.** `projectmp4 init` fetches, by `git clone --depth 1`:

| Pack | Repo | Contents |
|---|---|---|
| Cream of the Crop | `github.com/projectM-visualizer/presets-cream-of-the-crop` | 9,795 `.milk` in `Category/Subcategory/` |
| projectM classic | `github.com/projectM-visualizer/presets-projectm-classic` | 4,188 incl. **`bltc201/` (413 = the BLTC best-of)** and **`stock/` (557 = Winamp shipped)** |
| Textures | `github.com/projectM-visualizer/presets-milkdrop-texture-pack` | required; many presets render wrong without it |

Pin each to a commit in `presets/sources.toml`. `presets-community` is an **empty repo** — skip it.
Licensing: see §8; vendor the manifest, never the presets.

**Category structure is known and verified on disk**, which settles the `ambient` profile:

```
Geometric 41 subcats   Dancer 32   Fractal 26   Reaction 19   Hypnotic 8   Supernova 7
Fractal subcats include: Sierpinski, Nested Spiral, Mandelbox, Lattice, Trees, Core Tunnel,
  Radial, Horizon Mirror, Nested {Circle,Square,Hexagon,Pyramid,Ellipse}, Loops, Shine, …
```

**The scoring prototype exists and is the port source**, not a sketch:
`scratchpad/analysis/score.py` (BLTC + cross-reference) and `score_cotc.py` (full pack), with
`bltc_scores.json` and `cotc_scores.json` as recorded output. Port to Rust, then **assert the port
reproduces the Python ranking** on the same inputs — that is the acceptance test for `rank`.
Tokenizer rules as implemented: split each line at the first `=`, keep the right-hand side only for
lines matching `^(per_frame|per_pixel|per_point|wave_|shapecode|wavecode|warp_|comp_)\w*=`;
whole-word match with `(?<![a-z_])VAR(?![a-z_0-9])`; de-nest by subtracting the `_att` count from
the bare count. Shader detection: non-empty `warp_1=` or `comp_1=`, or the literal `shader_body`.

**Overlay reactivity reads from our own analysis pass, not from projectM.** The §6.3 analysis stage
already computes a per-frame low-band envelope; the overlay uses that. No projectM accessor needed,
and it keeps the overlay deterministic.

**Toolchain.** Rust is *not* installed on the machine — `rustup` install is a documented
prerequisite in the README, not something `build.rs` does. cmake 3.27.7 and a C++ toolchain are
present. "`cargo build` is the only command" means *after* prerequisites, and the README states them.

### 10.2 Decisions I made [my call]

- ~~Binary named `gf3`~~ — **overruled by Justin. The binary is `projectmp4`**, matching the crate
  and the repo. The pen name belongs on the watermark, not in the shell. No alias.
- **`dubstep` and `deathstep` are aliases of one profile.** Same categories, same weighting. Split
  them later if they diverge in practice.
- **N = 40 presets per profile.** Enough variety for a 12-switch track without repeats; small
  enough to render-verify the whole manifest in one pass.
- **Tier bonuses:** `+30%` of max bass score if in BLTC, `+20%` if in Winamp stock, `+20%` if in
  Geiss favorites. Multiplicative-free, additive, and written in `rank.rs` as named constants so
  they are visible and tunable. These numbers are arbitrary-but-stated: they order two *already
  human-curated* pools, they do not invent quality.
- **Genre weights:** `deathstep` bass×3 mid×1 treb×1; `dancehall` bass×2 mid×2 treb×1;
  `ambient` bass×1 mid×1 treb×1 and rank by `react` spread rather than bass peak.
- **Switch cadence:** minimum 8 s between preset switches; a detected drop may override the minimum
  down to 4 s. Hard cut on a drop, 2 s soft blend on a plain section change.
- **A track with no detected drops is valid**, not an error — ambient takes the section-change path
  only. If a track yields zero sections, fall back to a fixed 20 s rotation.
- **Preview stack: `winit` + `glutin` + `cpal`.** No SDL, so no system dependency, and the GL
  context setup is shared with the headless path.
- **Inputs: mp3, wav, flac, m4a/aac** via symphonia features. Suno exports wav as well as mp3 and
  the cost is a feature flag.
- **Acceptance gates split:** *automated* — frame count equals `ceil(duration × fps)`, A/V drift
  under one frame at the end of a 4-minute track, zero all-black frames, encoder exit 0, and the
  `rank` port reproducing the Python ranking. *Human* — Justin looks at the output and says whether
  it hits. A synthetic click-track fixture with known beat positions is built for the analysis
  stage so section detection is testable without a real song.
- **Performance:** the §9 timing is illustrative, not a requirement. Build the simple pipe first,
  measure, and only add double-buffered PBO readback if readback is shown to be the bottleneck.
  If x264 `-preset slow` is the limit, drop to `medium` before changing CRF.

### 10.3 agent-deck reuse — **decided: depend on its analysis crates, not just its cache**

Discovered after the first draft, and revised after Justin pointed out that stems will not always
exist: agent-deck contains not only a *cache* of analysis but the **Rust crates that produce it**.
projectmp4 depends on those directly, so it can analyse any track — cache or no cache, stems or no
stems. This supersedes §6.6's "write an RMS + spectral-flux detector" entirely.

**Crates consumed as path dependencies** (`/Users/tjw/Developer/agent-deck/native/`):

| Crate | Entry point | What it gives us |
|---|---|---|
| `deck-track-analysis` | `analyze(models: &Path, input: &Path) -> Result<serde_json::Value>` | bpm, `beats[]`, **`downbeats[]`**, spectral features, loop points — via `beat-this` 1.1.0 ONNX models under RTen |
| `deck-media-codec` | `decode_mono(&Path) -> Vec<f32>`, `visit_stereo(&Path, cb)` | symphonia decode at **48 kHz**, plus `ebur128` loudness |

Models ship at `native/track-analysis/models/{mel_spectrogram,beat_this_small}.onnx`; the path is
passed in, so projectmp4 takes it from config with that directory as the default.

**Two traps found by reading the source, both handled:**

1. **`deck-media-codec` does not enable symphonia's mp3 feature** — only
   `isomp4, ogg, pcm, wav`. A Suno **mp3 would fail to decode**, and `analyze()` calls
   `decode_mono` internally, so it would fail there too. *Resolution:* projectmp4 normalises every
   input to a temporary **48 kHz WAV via ffmpeg** before analysis and before the render driver.
   ffmpeg is already a dependency, it gives one code path for every container, and **agent-deck is
   not modified**. Do not "fix" this by editing agent-deck's Cargo.toml.
2. **Everything in agent-deck is 48 kHz**, and so are the htdemucs stems. §6.3's 44.1 kHz is
   wrong. **48 kHz is the pipeline rate throughout** — decode, analysis, stems, and the PCM fed to
   projectM. The only resample is ffmpeg's, at the normalise step.

**Signal priority for the visualizer driver**, best available wins:

```
1. cached htdemucs stems (drums + bass, summed)   ← naked kick, best reactivity
2. full mix at 48 kHz                             ← always available
   (both analysed by deck-track-analysis for beats/downbeats either way)
```

Preset switches land on **downbeats** from `beat-this`, never on an estimated boundary. Hard cut on
a downbeat that coincides with a large low-band energy step (a drop); 2 s soft blend otherwise.
`--driver stems|mix` overrides; `--analysis <dir>` points at a different models directory.

**Cache reuse stays, as a speed optimisation only.** Look up
`~/Library/Application Support/agent-deck/analysis/<sha256>.json` first and skip recomputation on a
hit. A miss costs time, never correctness. **Read-only — projectmp4 never writes into agent-deck's
directory.**

#### Cached corpus (for fixtures)

| Cached | Engine | Coverage |
|---|---|---|
| Beat grid + **downbeats** + BPM + cues | `beat-this-rs/1.1.0-small1-rten` (trained beat tracker) | 24/24 tracks |
| 4-stem separation: vocals / **drums** / **bass** / other, 48 kHz | `htdemucs-q8_0` | 20/24 complete |
| Loudness, mix-points | — | 70 / 159 files |
| Source audio (m4a) | — | 9 on disk |

Keyed by the track's **sha256**, which is also the filename: `analysis/<sha>.json`,
`beats/<sha>.json`, and a stem manifest under `sections/requests/` giving absolute stem paths.

**The audio muxed into the mp4 is always the full mix** — only the visualizer's driver changes.

**Fixtures available on disk, with stems and analysis:**

```
Ravezilla! (Dancehall Dub Rampage)   136 bpm  2:52   dancehall profile
Console Startup Error (Dancehall…)   136 bpm  2:45   dancehall profile
Sunday Trap Rite                      71 bpm  3:04   halftime / heavy
Bitcrushed Bats                      143 bpm  2:57   heavy
Three-Note Motif / Three-Note Signature  17s / 12s   unit-test fixtures
```

Still missing an **ambient** track and a true **deathstep** track — ask Justin before signing off
the analysis stage. The two Suno mp3s in §10.4 remain valid standalone (no-cache) test cases.

### 10.4 Answered by Justin

- **Watermark colour: cold white only, no accent.** Folded into §7.
- **Preset shortlist: commit it without review.** No approval step before the manifest lands. The
  ranking is community-curation-plus-measured-score and `projectmp4 rank` regenerates it, so it stays
  arguable after the fact. This removes the contact-sheet pass from scope.
- **Reference tracks: found and identified.** Not in `agent-deck` (its
  `examples/ui-wgpu/src/integrations/suno/tracks` is empty and it holds only dev audio fixtures).
  A filesystem-wide search for `comment` tags containing "made with suno" found exactly two:

  | Path | Title | Artist | Duration |
  |---|---|---|---|
  | `/Users/tjw/Downloads/track.mp3` | Sunday Console Rite | tjdubz | 3:39 |
  | `~/Library/Mobile Documents/com~apple~CloudDocs/Downloads/Dub T Summer.mp3` | Dub T Summer | tjdubz | 3:00 |

  **Useful side-finding that validates a design decision:** Suno writes real ID3 tags —
  `title`, `artist`, and a `comment` of the form
  `made with suno; created=<iso8601>; id=<uuid>`. So the §6.4 fallback chain
  (`--title` → ID3 → filename) will usually land on ID3 and need no flags at all for a Suno
  download. The `id` field is also a stable per-track identifier, usable as a cache key.

  Copy the chosen track into `samples/` (gitignored). **Neither of these two is clearly
  deathstep/dubstep from its title**, and drop detection is hardest on that genre, so ask Justin
  for one heavy track plus one ambient track as the no-drops-detected case before calling the
  analysis stage done. The synthetic click-track fixture is built regardless, for unit-testing
  section detection against known beat positions.
