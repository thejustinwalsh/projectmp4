// Candidate search and scoring adapted from PyMusicLooper (MIT), pinned under
// vendor/pymusiclooper. Original copyright (c) 2025 Hazem Nabil.
#include "deck_loops.h"
#include <algorithm>
#include <cmath>
#include <cstdio>
#include <exception>
#include <new>
#include <vector>

namespace {
struct Failure { deck_loops_status status; const char *message; };
void require(bool condition, const char *message) {
  if (!condition) throw Failure{DECK_LOOPS_INVALID, message};
}
void budget(bool condition) {
  if (!condition) throw Failure{DECK_LOOPS_BUDGET, "Loop analysis budget exceeded"};
}
double percentile(std::vector<double> values, double fraction) {
  std::sort(values.begin(), values.end());
  const double index = (values.size() - 1) * fraction;
  const auto lower = static_cast<size_t>(index);
  const auto upper = std::min(lower + 1, values.size() - 1);
  return values[lower] + (values[upper] - values[lower]) * (index - lower);
}
void prune(std::vector<deck_loop_pair> &pairs) {
  std::vector<double> notes, loudness;
  double max_note = 0, max_loudness = 0;
  for (const auto &pair : pairs) {
    max_note = std::max(max_note, pair.note_distance);
    max_loudness = std::max(max_loudness, pair.loudness_difference);
    if (pair.note_distance > 1e-3) notes.push_back(pair.note_distance);
    if (pair.loudness_difference > 1e-3) loudness.push_back(pair.loudness_difference);
  }
  const double note_threshold = notes.size() > 3 ? percentile(notes, .75) : max_note;
  const double db_threshold = std::max(.25, loudness.size() > 3 ? percentile(loudness, .5) : max_loudness);
  pairs.erase(std::remove_if(pairs.begin(), pairs.end(), [&](const auto &pair) {
    return pair.note_distance > note_threshold || pair.loudness_difference > db_threshold;
  }), pairs.end());
}
double similarity(const double *chroma, const std::vector<double> &norms,
                  uint32_t begin, uint32_t end, const std::vector<double> &weights,
                  double weight_sum, bool behind) {
  const auto test = static_cast<uint32_t>(weights.size());
  const auto frames = static_cast<uint32_t>(norms.size());
  const uint32_t available = behind ? std::min(test, begin) : std::min(test, frames - end);
  const uint32_t a = behind ? begin - available : begin;
  const uint32_t b = behind ? end - available : end;
  double score = 0;
  for (uint32_t offset = 0; offset < available; ++offset) {
    double dot = 0;
    for (uint32_t pitch = 0; pitch < 12; ++pitch)
      dot += chroma[(static_cast<size_t>(a) + offset) * 12 + pitch] * chroma[(static_cast<size_t>(b) + offset) * 12 + pitch];
    const double cosine = dot / std::max(norms[a + offset] * norms[b + offset], 1e-10);
    score += cosine * weights[behind ? test - 1 - offset : offset];
  }
  // Upstream pads unavailable frames with zeros, retaining the full denominator.
  return score / weight_sum;
}
std::vector<deck_loop_pair> rank(const double *chroma, const double *power,
                               const uint32_t *beats, const deck_loops_options &o, bool dj = false) {
  require(o.abi_version == DECK_LOOPS_ABI_VERSION, "Unsupported loop ABI");
  require(chroma && power && beats, "Missing feature buffer");
  require(o.frame_count > 0 && o.beat_count > 0, "Empty features or beats");
  require(o.min_loop_frames > 0 && o.min_loop_frames <= o.max_loop_frames, "Invalid loop duration range");
  require(o.test_frames > 0 && o.disable_pruning <= 1, "Invalid scoring options");
  require(o.max_candidates > 0 && o.max_pair_checks > 0 && o.max_score_frames > 0, "Missing analysis budgets");
  budget(o.frame_count <= 200000 && o.beat_count <= 20000 && o.test_frames <= 4096 && o.max_candidates <= 100000);
  budget(o.max_pair_checks <= 10000000 && o.max_score_frames <= 100000000);
  std::vector<double> norms(o.frame_count);
  for (uint32_t frame = 0; frame < o.frame_count; ++frame) {
    require(std::isfinite(power[frame]), "Non-finite power feature");
    double norm = 0;
    for (uint32_t pitch = 0; pitch < 12; ++pitch) {
      const double value = chroma[static_cast<size_t>(frame) * 12 + pitch];
      require(std::isfinite(value) && value >= 0 && value <= 1, "Chroma must be finite and normalized to [0,1]");
      norm += value * value;
    }
    norms[frame] = std::sqrt(norm);
  }
  for (uint32_t i = 0; i < o.beat_count; ++i)
    require(beats[i] < o.frame_count && (i == 0 || beats[i] > beats[i - 1]), "Beat indices must be in range and strictly increasing");
  uint64_t checks = 0;
  std::vector<deck_loop_pair> pairs;
  // Preserve the upstream end-major ordering so tied scores remain deterministic.
  for (uint32_t e = 0; e < o.beat_count; ++e) {
    const uint32_t end = beats[e];
    for (uint32_t s = 0; s < e; ++s) {
      budget(++checks <= o.max_pair_checks);
      const uint32_t steps = e - s;
      if (dj && steps != 4 && steps != 8 && steps != 16 && steps != 32 && steps != 64) continue;
      const uint32_t start = beats[s], length = end - start;
      if (length < o.min_loop_frames) break;
      if (length > o.max_loop_frames) continue;
      double distance = 0;
      for (uint32_t pitch = 0; pitch < 12; ++pitch) {
        const double difference = chroma[static_cast<size_t>(end) * 12 + pitch] - chroma[static_cast<size_t>(start) * 12 + pitch];
        distance += difference * difference;
      }
      distance = std::sqrt(distance);
      const double loudness = std::abs(power[end] - power[start]);
      if (distance <= norms[end] * .0875 && loudness <= .5) {
        budget(pairs.size() < o.max_candidates);
        pairs.push_back({start, end, distance, loudness, 0});
      }
    }
  }
  if (pairs.size() >= 100 && !o.disable_pruning) prune(pairs);
  budget(static_cast<uint64_t>(pairs.size()) * o.test_frames * 2 <= o.max_score_frames);
  std::vector<double> weights(o.test_frames);
  const double initial = std::max(2u, o.test_frames / 12);
  double sum = 0;
  for (uint32_t i = 0; i < o.test_frames; ++i) {
    weights[i] = o.test_frames == 1 ? initial : std::exp(std::log(initial) * (1.0 - static_cast<double>(i) / (o.test_frames - 1)));
    sum += weights[i];
  }
  for (auto &pair : pairs)
    pair.score = std::max(similarity(chroma, norms, pair.start_frame, pair.end_frame, weights, sum, false),
                          similarity(chroma, norms, pair.start_frame, pair.end_frame, weights, sum, true));
  std::stable_sort(pairs.begin(), pairs.end(), [](const auto &a, const auto &b) { return a.score > b.score; });
  return pairs;
}
}

extern "C" {
uint32_t deck_loops_version(void) { return DECK_LOOPS_ABI_VERSION; }
static deck_loops_status rank_boundary(const double *chroma, const double *power_db,
    const uint32_t *beats, const deck_loops_options *options, deck_loop_pair *output,
    uint32_t capacity, uint32_t *count, char *error, uint32_t error_capacity, bool dj) {
  if (count) *count = 0;
  if (error && error_capacity) error[0] = '\0';
  auto fail = [&](deck_loops_status status, const char *message) {
    if (error && error_capacity) std::snprintf(error, error_capacity, "%s", message);
    return status;
  };
  try {
    require(count && options, "Missing output count or options");
    require(output || capacity == 0, "Missing output storage");
    const auto pairs = rank(chroma, power_db, beats, *options, dj);
    *count = static_cast<uint32_t>(pairs.size());
    if (!output && capacity == 0) return DECK_LOOPS_OK;
    if (capacity < pairs.size()) return fail(DECK_LOOPS_CAPACITY, "Output capacity too small");
    if (!pairs.empty()) std::copy(pairs.begin(), pairs.end(), output);
    return DECK_LOOPS_OK;
  } catch (const Failure &failure) { return fail(failure.status, failure.message); }
    catch (const std::bad_alloc &) { return fail(DECK_LOOPS_BUDGET, "Loop analysis allocation failed"); }
    catch (const std::exception &) { return fail(DECK_LOOPS_INTERNAL, "Loop analysis failed"); }
    catch (...) { return fail(DECK_LOOPS_INTERNAL, "Unknown loop analysis failure"); }
}
deck_loops_status deck_loops_rank(const double *chroma, const double *power_db,
    const uint32_t *beats, const deck_loops_options *options, deck_loop_pair *output,
    uint32_t capacity, uint32_t *count, char *error, uint32_t error_capacity) {
  return rank_boundary(chroma, power_db, beats, options, output, capacity, count, error, error_capacity, false);
}
deck_loops_status deck_loops_rank_dj(const double *chroma, const double *power_db,
    const uint32_t *beats, const deck_loops_options *options, deck_loop_pair *output,
    uint32_t capacity, uint32_t *count, char *error, uint32_t error_capacity) {
  return rank_boundary(chroma, power_db, beats, options, output, capacity, count, error, error_capacity, true);
}
}
