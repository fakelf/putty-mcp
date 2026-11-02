#include "ring.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#define CHECK(x) do { if (!(x)) { fprintf(stderr,"FAILED line %d: %s\n",__LINE__,#x); exit(1); } } while(0)
static DWORD WINAPI consumer(void *ctx)
{
    AiRing *r=ctx; AiChunk out; uint64_t last=0;
    while (last<64*1024*1024) {
        if (ai_ring_pop(r,&out)) {
            CHECK(out.sequence>=last); CHECK(out.length<=AI_CHUNK);
            CHECK(out.data[0]==0x5a && out.data[out.length-1]==0x5a);
            last=out.sequence+out.length;
        } else if (ai_ring_drained_sequence(r)>=64*1024*1024) break;
        else SwitchToThread();
    }
    return 0;
}
int main(void)
{
    AiRing r; AiChunk out; unsigned char data[AI_CHUNK];
    LARGE_INTEGER f,a,b; double seconds; unsigned i; HANDLE thread;
    memset(data,0x5a,sizeof(data));
    CHECK(ai_ring_init(&r,2));
    ai_ring_push(&r,data,sizeof(data)); ai_ring_push(&r,data,sizeof(data)); ai_ring_push(&r,data,sizeof(data));
    CHECK(r.dropped==AI_CHUNK); CHECK(ai_ring_pop(&r,&out)); CHECK(out.sequence==0);
    CHECK(ai_ring_pop(&r,&out)); CHECK(out.sequence==AI_CHUNK); CHECK(!ai_ring_pop(&r,&out));
    CHECK(ai_ring_drained_sequence(&r)==3*AI_CHUNK);
    ai_ring_push(&r,data,sizeof(data)); CHECK(ai_ring_pop(&r,&out)); CHECK(out.sequence==3*AI_CHUNK);
    ai_ring_free(&r);
    CHECK(ai_ring_init(&r,128));
    thread=CreateThread(NULL,0,consumer,&r,0,NULL); CHECK(thread!=NULL);
    for(i=0;i<4096;i++) ai_ring_push(&r,data,sizeof(data));
    CHECK(WaitForSingleObject(thread,10000)==WAIT_OBJECT_0); CloseHandle(thread); ai_ring_free(&r);
    CHECK(ai_ring_init(&r,128)); QueryPerformanceFrequency(&f); QueryPerformanceCounter(&a);
    for(i=0;i<65536;i++) { ai_ring_push(&r,data,sizeof(data)); CHECK(ai_ring_pop(&r,&out)); }
    QueryPerformanceCounter(&b); seconds=(double)(b.QuadPart-a.QuadPart)/f.QuadPart;
    printf("{\"test\":\"native_spsc\",\"passed\":true,\"bytes\":1073741824,\"seconds\":%.6f,\"MiB_per_second\":%.2f,\"callback_us\":%.3f,\"ring_bytes\":%llu}\n",
        seconds,1024.0/seconds,seconds*1e6/65536,(unsigned long long)(r.slots*sizeof(AiChunk)));
    ai_ring_free(&r); return 0;
}
