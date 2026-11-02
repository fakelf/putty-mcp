#include "ring.h"
#include <string.h>
int ai_ring_init(AiRing *r, size_t slots)
{
    memset(r, 0, sizeof(*r));
    if (slots < 2 || slots > 1024) return 0;
    r->slots = slots;
    r->chunks = HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(AiChunk)*slots);
    return r->chunks != NULL;
}
void ai_ring_free(AiRing *r) { HeapFree(GetProcessHeap(), 0, r->chunks); }
int ai_ring_push(AiRing *r, const void *input, size_t size)
{
    const unsigned char *data = input;
    size_t copied = 0;
    LONG64 w = InterlockedCompareExchange64(&r->written, 0, 0);
    int empty = w == InterlockedCompareExchange64(&r->consumed, 0, 0);
    /* At most 64 KiB extra work per PuTTY output callback, even for huge input. */
    while (copied < size && copied < 65536) {
        size_t n = size-copied;
        AiChunk *chunk;
        if (w-InterlockedCompareExchange64(&r->consumed, 0, 0) >= (LONG64)r->slots) break;
        if (n > AI_CHUNK) n = AI_CHUNK;
        chunk = &r->chunks[(size_t)w % r->slots];
        chunk->sequence = r->sequence+copied;
        chunk->length = n;
        memcpy(chunk->data, data+copied, n);
        copied += n;
        InterlockedExchange64(&r->written, ++w);
    }
    if (size > copied) InterlockedAdd64(&r->dropped, (LONG64)(size-copied));
    r->sequence += size;
    InterlockedExchange64(&r->produced, (LONG64)r->sequence);
    return empty && copied != 0;
}
int ai_ring_pop(AiRing *r, AiChunk *out)
{
    LONG64 c = InterlockedCompareExchange64(&r->consumed, 0, 0);
    if (c == InterlockedCompareExchange64(&r->written, 0, 0)) return 0;
    memcpy(out, &r->chunks[(size_t)c % r->slots], sizeof(*out));
    InterlockedExchange64(&r->consumed, c+1);
    return 1;
}
uint64_t ai_ring_drained_sequence(AiRing *r)
{
    LONG64 seq = InterlockedCompareExchange64(&r->produced, 0, 0);
    if (InterlockedCompareExchange64(&r->consumed, 0, 0) !=
        InterlockedCompareExchange64(&r->written, 0, 0)) return 0;
    return (uint64_t)seq;
}

