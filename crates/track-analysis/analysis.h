#ifndef DECK_ANALYSIS_H
#define DECK_ANALYSIS_H
#include <stdint.h>
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif
uint32_t deck_analysis_version(void);
/* Blocking worker-only call; UTF-8 NUL-terminated paths. Caller owns all buffers.
   JSON bytes are not NUL terminated; written receives their exact byte count.
   -1 clears written and supplies a bounded error. No partial JSON is written. */
int32_t deck_analyze_file(const char *models, const char *input, uint8_t *output,
    size_t capacity, size_t *written, uint8_t *error, size_t error_capacity);
/* One durable work unit per call. 0 = complete JSON, 1 = yielded JSON,
   -1 = error. Same buffer ownership and bounds as deck_analyze_file.
   Reuse checkpoint_dir to continue; slice_ms is a soft time budget. */
int32_t deck_analyze_slice(const char *models, const char *input,
    const char *checkpoint_dir, uint64_t slice_ms, uint8_t *output,
    size_t capacity, size_t *written, uint8_t *error, size_t error_capacity);
#ifdef __cplusplus
}
#endif
#endif
