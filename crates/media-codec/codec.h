#pragma once
#include <cstddef>
#include <cstdint>
extern "C" {
struct DeckDecodedAudio { const float* samples; uint64_t frames; void* owner; };
int deck_decode_file(const char* path, DeckDecodedAudio* output, char* error, size_t capacity);
void deck_decode_free(void* owner);
}
