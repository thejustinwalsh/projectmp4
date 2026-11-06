#ifndef DECK_LOOPS_H
#define DECK_LOOPS_H
#include <stdint.h>
#if defined(_WIN32)
# if defined(DECK_LOOPS_BUILD)
#  define DECK_LOOPS_API __declspec(dllexport)
# else
#  define DECK_LOOPS_API __declspec(dllimport)
# endif
#else
# define DECK_LOOPS_API __attribute__((visibility("default")))
#endif
#ifdef __cplusplus
extern "C" {
#endif

#define DECK_LOOPS_ABI_VERSION 1u
typedef enum {
  DECK_LOOPS_OK = 0, DECK_LOOPS_INVALID = 1, DECK_LOOPS_BUDGET = 2,
  DECK_LOOPS_CAPACITY = 3, DECK_LOOPS_INTERNAL = 4
} deck_loops_status;

/* Feature-frame indices, not audio sample offsets. All storage is caller-owned.
   Chroma: contiguous float64 [frame_count,12], finite and normalized to [0,1].
   power_db: finite float64 [frame_count], max frequency-bin dB per frame.
   beats: strictly increasing uint32 indices into those same feature frames.
   Feature extraction and beat detection are separate stages. */
typedef struct {
  uint32_t abi_version;
  uint32_t frame_count;
  uint32_t beat_count;
  uint32_t min_loop_frames;
  uint32_t max_loop_frames;
  uint32_t test_frames;
  uint32_t disable_pruning;
  uint32_t max_candidates;
  uint64_t max_pair_checks;
  uint64_t max_score_frames;
} deck_loops_options;

typedef struct {
  uint32_t start_frame;
  uint32_t end_frame;
  double note_distance;
  double loudness_difference;
  double score;
} deck_loop_pair;

DECK_LOOPS_API uint32_t deck_loops_version(void);
/* Query count with output=NULL, capacity=0. A short buffer returns CAPACITY and
   writes no pairs. Every failure sets count=0, except CAPACITY which returns
   the required count. Budget exhaustion returns no partial/ranked result.
   No exception crosses this boundary; no retained pointers; thread-safe.
   Run on an analysis worker, never the realtime audio callback. */
DECK_LOOPS_API deck_loops_status deck_loops_rank(
  const double *chroma, const double *power_db, const uint32_t *beats,
  const deck_loops_options *options, deck_loop_pair *output,
  uint32_t capacity, uint32_t *count, char *error, uint32_t error_capacity);

/* Product adaptation: restrict to 4/8/16/32/64 beat-index intervals before
   scoring. Beat counts do not imply a known meter. Ownership/errors as above. */
DECK_LOOPS_API deck_loops_status deck_loops_rank_dj(
  const double *chroma, const double *power_db, const uint32_t *beats,
  const deck_loops_options *options, deck_loop_pair *output,
  uint32_t capacity, uint32_t *count, char *error, uint32_t error_capacity);
#ifdef __cplusplus
}
#endif
#endif
