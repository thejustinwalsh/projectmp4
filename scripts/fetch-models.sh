#!/usr/bin/env bash
#
# Fetch the third-party model weights projectmp4 loads at runtime.
#
# These are not committed here: a public repository should carry what is needed
# to *build* it, not a copy of every artifact it loads, and these are 73 MB of
# bytes nobody edits. The pins below are what make that safe — an exact commit
# or revision and an exact sha256, so a fetch either reproduces the bytes the
# project was tested against or fails.
#
#   ./scripts/fetch-models.sh              # every group
#   ./scripts/fetch-models.sh beat         # just the beat models (11 MB)
#   ./scripts/fetch-models.sh stems        # just the separation model (62 MB)
#   ./scripts/fetch-models.sh all --force  # re-fetch even if present and valid
#
# You should not have to run this by hand. `build.rs` fetches the `beat` group
# when it is missing, because nothing builds without it, and the `stems` group
# is fetched on first use of `--clip drop` or `--clip intro`, because nothing
# else needs 62 MB. Running it yourself just does that work earlier.
#
# Idempotent: a file already present with the right hash is left alone.

set -euo pipefail

# <group>|<destination>|<url>|<sha256>|<bytes>
#
# beat: danigb/beat-this-rs, tag v1.1.0, pinned to the commit rather than the
# tag because a tag can be moved to point at different bytes and a commit
# cannot.
#
# stems: the htdemucs GGUF conversion audio.cpp loads, pinned to a HuggingFace
# revision. `vendor/stems-runtime/runtime.json` carries the same sha256 and the
# worker verifies it again at load, so this file is checked twice.
MODELS=(
  "beat|crates/track-analysis/models/beat_this_small.onnx|https://raw.githubusercontent.com/danigb/beat-this-rs/1ae768e78f1ad83b0ed3886241dc29ffde853c40/models/beat_this_small.onnx|a5f8d39d989f31859454ba27afe61c5317ca95e4d9373e6853e5361b8937172f|10555592"
  "beat|crates/track-analysis/models/mel_spectrogram.onnx|https://raw.githubusercontent.com/danigb/beat-this-rs/1ae768e78f1ad83b0ed3886241dc29ffde853c40/models/mel_spectrogram.onnx|fdd59e65c515331308e4c8841edf99972deca646bdf6197744c2a5b7755e3de9|270742"
  "stems|vendor/stems-runtime/htdemucs-q8_0.gguf|https://huggingface.co/audio-cpp/audio.cpp-gguf/resolve/4c9a3098d552a0f3d9b2adc11638a11022c07d7e/HTDemucs-GGUF/htdemucs-q8_0.gguf|b0f532ac6e5f373aeb11fa0df73253251e133832d9c8b9942dc58f50bc5b4388|61940768"
)

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
want_group="all"
force=0
for arg in "$@"; do
  case "$arg" in
    --force) force=1 ;;
    all|beat|stems) want_group="$arg" ;;
    *) echo "unknown argument $arg; expected all, beat, stems or --force" >&2; exit 2 ;;
  esac
done

sha256_of() {
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  elif command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else echo "need shasum or sha256sum to verify downloads" >&2; exit 1
  fi
}

fetched=0 kept=0
for entry in "${MODELS[@]}"; do
  IFS='|' read -r group dest url want bytes <<<"$entry"
  [[ "$want_group" == "all" || "$want_group" == "$group" ]] || continue
  out="$root/$dest"

  if [[ $force -eq 0 && -f "$out" ]]; then
    if [[ "$(sha256_of "$out")" == "$want" ]]; then
      echo "ok       $dest"
      kept=$((kept + 1))
      continue
    fi
    echo "replacing $dest (hash does not match the pin)"
  fi

  mkdir -p "$(dirname "$out")"
  tmp="$out.part"
  echo "fetching $dest ($(( bytes / 1000000 )).$(( bytes / 100000 % 10 )) MB)"
  # --fail so an error page is never mistaken for a model.
  curl --fail --silent --show-error --location --output "$tmp" "$url"

  got="$(sha256_of "$tmp")"
  if [[ "$got" != "$want" ]]; then
    rm -f "$tmp"
    echo >&2
    echo "sha256 mismatch for $dest" >&2
    echo "  expected $want" >&2
    echo "  got      $got" >&2
    echo "  from     $url" >&2
    echo >&2
    echo "Nothing was written. A pinned commit or revision should serve fixed" >&2
    echo "bytes, so this means the source changed or the download was corrupted." >&2
    echo "Do not bypass this check: these weights decide the beat grid every" >&2
    echo "switch time in a render is built on, and the stems a clip is cut from." >&2
    exit 1
  fi

  mv "$tmp" "$out"
  fetched=$((fetched + 1))
done

echo
echo "models ready ($fetched fetched, $kept already present)"
