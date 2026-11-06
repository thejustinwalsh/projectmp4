#include "deck_loops.h"
#include <assert.h>
#include <math.h>
#include <stddef.h>
#include <string.h>

int main(void) {
  _Static_assert(sizeof(deck_loops_options) == 48, "Options ABI size");
  _Static_assert(offsetof(deck_loops_options, max_pair_checks) == 32, "Options ABI alignment");
  _Static_assert(sizeof(deck_loop_pair) == 32, "Pair ABI size");
  double chroma[16 * 12], power[16] = {0};
  uint32_t beats[4] = {0, 4, 8, 12}, count = 123;
  for (int i = 0; i < 16 * 12; ++i) chroma[i] = 1;
  deck_loops_options options = {1, 16, 4, 4, 8, 4, 0, 100, 100, 10000};
  deck_loop_pair result[8];
  char error[128];
  assert(deck_loops_version() == 1);
  assert(deck_loops_rank(chroma, power, beats, &options, NULL, 0, &count, error, sizeof error) == DECK_LOOPS_OK);
  assert(count == 5);
  memset(result, 0x42, sizeof result);
  deck_loop_pair before[8]; memcpy(before, result, sizeof result);
  assert(deck_loops_rank(chroma, power, beats, &options, result, 1, &count, error, sizeof error) == DECK_LOOPS_CAPACITY);
  assert(count == 5 && memcmp(before, result, sizeof result) == 0);
  assert(deck_loops_rank(chroma, power, beats, &options, result, 8, &count, error, sizeof error) == DECK_LOOPS_OK);
  assert(result[0].start_frame == 0 && result[0].end_frame == 4 && fabs(result[0].score - 1) < 1e-12);
  assert(deck_loops_rank_dj(chroma, power, beats, &options, result, 8, &count, error, sizeof error) == DECK_LOOPS_OK && count == 0);
  options.max_pair_checks = 1;
  assert(deck_loops_rank(chroma, power, beats, &options, result, 8, &count, error, sizeof error) == DECK_LOOPS_BUDGET && count == 0);
  options.max_pair_checks = 100; options.max_candidates = 1;
  assert(deck_loops_rank(chroma, power, beats, &options, result, 8, &count, error, sizeof error) == DECK_LOOPS_BUDGET && count == 0);
  options.max_candidates = 100; options.max_score_frames = 1;
  assert(deck_loops_rank(chroma, power, beats, &options, result, 8, &count, error, sizeof error) == DECK_LOOPS_BUDGET && count == 0);
  options.max_score_frames = 10000;
  beats[1] = 0;
  assert(deck_loops_rank(chroma, power, beats, &options, result, 8, &count, error, sizeof error) == DECK_LOOPS_INVALID && count == 0);
  beats[1] = 4; chroma[0] = NAN;
  assert(deck_loops_rank(chroma, power, beats, &options, result, 8, &count, error, sizeof error) == DECK_LOOPS_INVALID && count == 0);
  chroma[0] = 1; power[0] = INFINITY;
  assert(deck_loops_rank(chroma, power, beats, &options, result, 8, &count, error, sizeof error) == DECK_LOOPS_INVALID && count == 0);
  power[0] = 0; options.abi_version = 999;
  assert(deck_loops_rank(chroma, power, beats, &options, result, 8, &count, error, sizeof error) == DECK_LOOPS_INVALID && count == 0);
  assert(deck_loops_rank(NULL, NULL, NULL, NULL, NULL, 0, &count, error, sizeof error) == DECK_LOOPS_INVALID && count == 0);
  return 0;
}
