#!/usr/bin/env bash
#
# Fetch the beat-tracking model weights projectmp4 needs at runtime.
#
# These are third-party ONNX exports. They are not committed here: they are
# 10 MB of bytes nobody edits, and a public repository should carry what is
# needed to *build* it, not a copy of every artifact it loads. The pins below
# are what make that safe — an exact commit and an exact hash, so a fetch
# either reproduces the bytes the project was tested against or fails.
#
#   ./scripts/fetch-models.sh            # fetch what is missing, verify all
#   ./scripts/fetch-models.sh --force    # re-fetch even if present and valid
#
# Idempotent: a file already present with the right hash is left alone.

set -euo pipefail

# danigb/beat-this-rs, tag v1.1.0. Pinned to the commit rather than the tag:
# a tag can be moved to point at different bytes, a commit cannot.
REPO="danigb/beat-this-rs"
COMMIT="1ae768e78f1ad83b0ed3886241dc29ffde853c40"

# <destination>|<path in that repo>|<sha256>|<bytes>
MODELS=(
  "crates/track-analysis/models/beat_this_small.onnx|models/beat_this_small.onnx|a5f8d39d989f31859454ba27afe61c5317ca95e4d9373e6853e5361b8937172f|10555592"
  "crates/track-analysis/models/mel_spectrogram.onnx|models/mel_spectrogram.onnx|fdd59e65c515331308e4c8841edf99972deca646bdf6197744c2a5b7755e3de9|270742"
)

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
force=0
[[ "${1:-}" == "--force" ]] && force=1

sha256_of() {
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  elif command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else echo "need shasum or sha256sum to verify downloads" >&2; exit 1
  fi
}

fetched=0 kept=0
for entry in "${MODELS[@]}"; do
  IFS='|' read -r dest src want bytes <<<"$entry"
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
  url="https://raw.githubusercontent.com/$REPO/$COMMIT/$src"
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
    echo "Nothing was written. The pinned commit should serve fixed bytes, so" >&2
    echo "this means the source changed or the download was corrupted. Do not" >&2
    echo "bypass this check: the beat grid these weights produce is what every" >&2
    echo "switch time in a render is built on." >&2
    exit 1
  fi

  mv "$tmp" "$out"
  fetched=$((fetched + 1))
done

echo
echo "models ready in crates/track-analysis/models ($fetched fetched, $kept already present)"
