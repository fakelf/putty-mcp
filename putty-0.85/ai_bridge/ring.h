#ifndef PUTTY_AI_RING_H
#define PUTTY_AI_RING_H
#include <windows.h>
#include <stdint.h>
#include <stddef.h>
#define AI_CHUNK 16384
typedef struct AiChunk { uint64_t sequence; size_t length; unsigned char data[AI_CHUNK]; } AiChunk;
typedef struct AiRing {
    __declspec(align(64)) volatile LONG64 written;
    __declspec(align(64)) volatile LONG64 consumed;
    volatile LONG64 produced;
    uint64_t sequence; /* producer only */
    volatile LONG64 dropped;
    size_t slots;
    AiChunk *chunks;
} AiRing;
int ai_ring_init(AiRing *, size_t slots);
void ai_ring_free(AiRing *);
int ai_ring_push(AiRing *, const void *, size_t);
int ai_ring_pop(AiRing *, AiChunk *);
uint64_t ai_ring_drained_sequence(AiRing *);
#endif

