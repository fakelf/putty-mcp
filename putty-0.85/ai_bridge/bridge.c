/* PuTTY AI sidecar. No PuTTY objects cross the GUI/worker boundary.
 * GUI capture is a bounded SPSC copy. Only this worker performs pipe I/O.
 * Memory stays alive until both GUI owner and worker release their references.
 */
#define _WIN32_WINNT 0x0601
#include "bridge.h"
#include "ring.h"
#include "approval.h"
#include "command.h"
#include <sddl.h>
#include <bcrypt.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <malloc.h>

#define F_READ 1
#define F_CONNECTED 2
#define F_CONTROL 8
#define F_CUSTOM_CAPABLE 16
#define F_CUSTOM_CONTROL 32
#define AI_CONTROL_MASK (F_CONTROL|F_CUSTOM_CONTROL)
#define FRAME_MAX 131072
typedef struct AiHeader {
    char magic[4]; uint16_t version, kind; uint32_t length, flags;
    unsigned char session[16], request[16]; uint64_t sequence, epoch;
} AiHeader;
typedef char header_must_be_64[(sizeof(AiHeader)==64)?1:-1];
struct AiBridge {
    AiRing ring;
    HANDLE stop, wake;
    HWND hwnd;
    volatile LONG references, flags, online, revoke, inbox_state;
    volatile LONG64 epoch;
    unsigned char session[16], client[16]; /* client owned by GUI */
    AiHeader incoming;
    unsigned char payload[AI_COMMAND_MAX+21]; /* custom: client[16], timeout[4], UTF-8 script, NUL */
    DWORD ack;
    ULONGLONG lease_until, command_until; /* GUI only */
    AiHeader outgoing_ack; /* immutable while inbox_state == 3 */
    HWND approval; /* GUI only; modeless, never holds the PuTTY event loop */
};
static LONG load(volatile LONG *p) { return InterlockedCompareExchange(p,0,0); }
static uint64_t epoch(AiBridge *b) { return (uint64_t)InterlockedCompareExchange64(&b->epoch,0,0); }
static void release(AiBridge *b)
{
    if (!InterlockedDecrement(&b->references)) {
        ai_ring_free(&b->ring); CloseHandle(b->stop); CloseHandle(b->wake);
        SecureZeroMemory(b,sizeof(*b)); _aligned_free(b);
    }
}
static int identity(DWORD pid, PSID own_sid)
{
    HANDLE p=OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION,FALSE,pid), token=NULL;
    DWORD n=0; TOKEN_USER *u=NULL; int result=0;
    if (!p) return 0;
    if (!OpenProcessToken(p,TOKEN_QUERY,&token)) goto done;
    GetTokenInformation(token,TokenUser,NULL,0,&n);
    u=HeapAlloc(GetProcessHeap(),0,n);
    if (u && GetTokenInformation(token,TokenUser,u,n,&n)) result=EqualSid(own_sid,u->User.Sid);
  done:
    if (u) HeapFree(GetProcessHeap(),0,u);
    if (token) CloseHandle(token); CloseHandle(p); return result;
}
static int own_pipe(wchar_t *name, size_t cap, TOKEN_USER **user)
{
    HANDLE token; DWORD n=0; LPWSTR sid=NULL; int ok=0;
    if (!OpenProcessToken(GetCurrentProcess(),TOKEN_QUERY,&token)) return 0;
    GetTokenInformation(token,TokenUser,NULL,0,&n);
    *user=HeapAlloc(GetProcessHeap(),0,n);
    if (*user && GetTokenInformation(token,TokenUser,*user,n,&n) &&
        ConvertSidToStringSidW((*user)->User.Sid,&sid)) {
        ok=swprintf_s(name,cap,L"\\\\.\\pipe\\putty-ai-v2-%s",sid)>0;
        LocalFree(sid);
    }
    CloseHandle(token); return ok;
}
static int transfer(AiBridge *b, HANDLE pipe, void *data, DWORD count, int writing)
{
    OVERLAPPED ov; HANDLE events[2]; DWORD n, error; BOOL ok;
    unsigned char *p=data;
    memset(&ov,0,sizeof(ov)); ov.hEvent=CreateEvent(NULL,TRUE,FALSE,NULL);
    if (!ov.hEvent) return 0;
    events[0]=b->stop; events[1]=ov.hEvent;
    while (count) {
        ResetEvent(ov.hEvent);
        ok=writing ? WriteFile(pipe,p,count,&n,&ov) : ReadFile(pipe,p,count,&n,&ov);
        if (!ok) {
            error=GetLastError();
            if (error!=ERROR_IO_PENDING) { CloseHandle(ov.hEvent); return 0; }
            if (WaitForMultipleObjects(2,events,FALSE,2000)!=WAIT_OBJECT_0+1) {
                CancelIoEx(pipe,&ov);
                /* Worker only: reap cancellation before stack OVERLAPPED disappears. */
                GetOverlappedResult(pipe,&ov,&n,TRUE);
                CloseHandle(ov.hEvent); return 0;
            }
            if (!GetOverlappedResult(pipe,&ov,&n,FALSE)) { CloseHandle(ov.hEvent); return 0; }
        }
        if (!n) { CloseHandle(ov.hEvent); return 0; }
        p+=n; count-=n;
    }
    CloseHandle(ov.hEvent); return 1;
}
static void header(AiBridge *b,AiHeader *h,unsigned kind,unsigned length)
{
    memset(h,0,sizeof(*h)); memcpy(h->magic,"PAI1",4); h->version=1;
    h->kind=(uint16_t)kind; h->length=length;
    h->flags=(uint32_t)load(&b->flags); memcpy(h->session,b->session,16); h->epoch=epoch(b);
}
static int exchange(AiBridge *b,HANDLE pipe,AiHeader *h,void *payload)
{
    AiHeader reply; unsigned char data[AI_COMMAND_MAX+21];
    if (!transfer(b,pipe,h,sizeof(*h),1) ||
        (h->length && !transfer(b,pipe,payload,h->length,1)) ||
        !transfer(b,pipe,&reply,sizeof(reply),0)) return 0;
    if (memcmp(reply.magic,"PAI1",4) || reply.version!=1 || reply.length>=sizeof(data) ||
        memcmp(reply.session,b->session,16)) return 0;
    if (reply.length && !transfer(b,pipe,data,reply.length,0)) return 0;
    if (reply.kind==12) {
        InterlockedAnd(&b->flags,~AI_CONTROL_MASK); InterlockedExchange(&b->revoke,1);
        InterlockedCompareExchange(&b->inbox_state,5,4); /* invalidate open consent before GUI sees notification */
        PostMessage(b->hwnd,WM_AI_BRIDGE,0,0);
    } else if (reply.kind==10 || reply.kind==11 || reply.kind==13 || reply.kind==14) {
        if (reply.epoch!=epoch(b)) return 1; /* stale local authorization generation */
        if (((reply.kind==10 || reply.kind==13) && reply.length!=16) ||
            (reply.kind==11 && reply.length!=24) ||
            (reply.kind==14 && (reply.length<=20 || !ai_command_validate((char *)data+20,reply.length-20)))) return 0;
        data[reply.length]=0;
        if (InterlockedCompareExchange(&b->inbox_state,1,0)==0) {
            b->incoming=reply; memcpy(b->payload,data,reply.length);
            b->payload[reply.length]=0;
            InterlockedExchange(&b->inbox_state,2); PostMessage(b->hwnd,WM_AI_BRIDGE,0,0);
        } else return 0; /* fail closed; broker marks uncertain requests unknown */
    } else if (reply.kind!=0) return 0;
    return 1;
}
static DWORD WINAPI worker(void *ctx)
{
    AiBridge *b=ctx; wchar_t name[256]; TOKEN_USER *user=NULL;
    HANDLE pipe=INVALID_HANDLE_VALUE, events[2]={b->stop,b->wake}; DWORD backoff=250;
    AiHeader h; AiChunk chunk; unsigned char hello[20], status[28], active_client[16]={0};
    FILETIME c,e,k,u; DWORD pid=GetCurrentProcessId(); uint64_t start,hwnd=(uint64_t)(uintptr_t)b->hwnd;
    if (!own_pipe(name,256,&user)) goto done;
    if (!GetProcessTimes(GetCurrentProcess(),&c,&e,&k,&u)) goto done;
    start=((uint64_t)c.dwHighDateTime<<32)|c.dwLowDateTime;
    memcpy(hello,&pid,4); memcpy(hello+4,&start,8); memcpy(hello+12,&hwnd,8);
    while (WaitForSingleObject(b->stop,0)!=WAIT_OBJECT_0) {
        DWORD peer=0;
        pipe=CreateFileW(name,GENERIC_READ|GENERIC_WRITE,0,NULL,OPEN_EXISTING,
                         FILE_FLAG_OVERLAPPED|SECURITY_SQOS_PRESENT|SECURITY_IDENTIFICATION,NULL);
        if (pipe==INVALID_HANDLE_VALUE || !GetNamedPipeServerProcessId(pipe,&peer) || !identity(peer,user->User.Sid)) goto offline;
        header(b,&h,1,20); h.sequence=0;
        if (!exchange(b,pipe,&h,hello)) goto offline;
        InterlockedExchange(&b->online,1); backoff=250;
        while (WaitForSingleObject(b->stop,0)!=WAIT_OBJECT_0) {
            unsigned batch=0;
            while (batch++<16 && ai_ring_pop(&b->ring,&chunk)) {
                header(b,&h,2,(unsigned)chunk.length); h.sequence=chunk.sequence;
                if (!exchange(b,pipe,&h,chunk.data)) goto offline;
            }
            header(b,&h,3,28); h.sequence=ai_ring_drained_sequence(&b->ring);
            memset(status,0,sizeof(status));
            if (load(&b->inbox_state)==3) {
                if ((b->outgoing_ack.kind==10 || b->outgoing_ack.kind==13) && b->ack==1) memcpy(active_client,b->payload,16);
                if (b->outgoing_ack.kind==10 || b->outgoing_ack.kind==13) memcpy(status,b->payload,16);
                else memcpy(status,active_client,16);
                memcpy(h.request,b->outgoing_ack.request,16);
                memcpy(status+16,&b->ack,4);
                InterlockedExchange(&b->inbox_state,0);
            } else memcpy(status,active_client,16);
            { LONG64 dropped=InterlockedCompareExchange64(&b->ring.dropped,0,0); memcpy(status+20,&dropped,8); }
            if (!exchange(b,pipe,&h,status)) goto offline;
            if (InterlockedCompareExchange64(&b->ring.written,0,0)!=InterlockedCompareExchange64(&b->ring.consumed,0,0)) continue;
            if (WaitForMultipleObjects(2,events,FALSE,250)==WAIT_OBJECT_0) break;
        }
      offline:
        InterlockedExchange(&b->online,0); InterlockedAnd(&b->flags,~AI_CONTROL_MASK);
        InterlockedExchange(&b->revoke,1); PostMessage(b->hwnd,WM_AI_BRIDGE,0,0);
        if (pipe!=INVALID_HANDLE_VALUE) { CancelIoEx(pipe,NULL); CloseHandle(pipe); pipe=INVALID_HANDLE_VALUE; }
        if (WaitForSingleObject(b->stop,backoff)==WAIT_OBJECT_0) break;
        if (backoff<5000) backoff=backoff*2>5000?5000:backoff*2;
    }
  done:
    if (pipe!=INVALID_HANDLE_VALUE) CloseHandle(pipe);
    if (user) HeapFree(GetProcessHeap(),0,user);
    release(b); return 0;
}
static AiBridge *start(HWND hwnd)
{
    AiBridge *b=_aligned_malloc(sizeof(*b),64); HANDLE thread;
    wchar_t setting[32]; unsigned slots=128;
    if (!b) return NULL;
    memset(b,0,sizeof(*b));
    if (GetEnvironmentVariableW(L"PUTTY_AI_RING_SLOTS",setting,32)) {
        unsigned requested=(unsigned)_wtoi(setting); if (requested>=16 && requested<=1024) slots=requested;
    }
    b->references=1;
    b->stop=CreateEvent(NULL,TRUE,FALSE,NULL); b->wake=CreateEvent(NULL,FALSE,FALSE,NULL);
    if (!b->stop || !b->wake || !ai_ring_init(&b->ring,slots) ||
        BCryptGenRandom(NULL,b->session,16,BCRYPT_USE_SYSTEM_PREFERRED_RNG)!=0) {
        if (b->ring.chunks) ai_ring_free(&b->ring);
        if (b->stop) CloseHandle(b->stop); if (b->wake) CloseHandle(b->wake);
        _aligned_free(b); return NULL;
    }
    b->hwnd=hwnd; b->flags=F_READ|F_CONNECTED|F_CUSTOM_CAPABLE; b->epoch=1; b->references=2;
    thread=CreateThread(NULL,0,worker,b,0,NULL);
    if (!thread) { b->references=1; release(b); return NULL; }
    CloseHandle(thread); SetTimer(hwnd,AI_TIMER,250,NULL); return b;
}
void ai_bridge_stop(AiBridge **owner)
{
    AiBridge *b=*owner; if (!b) return;
    *owner=NULL; InterlockedExchange(&b->flags,0); KillTimer(b->hwnd,AI_TIMER);
    if (b->approval) DestroyWindow(b->approval);
    PostMessage(b->hwnd,WM_AI_BRIDGE,0,0);
    SetEvent(b->stop); release(b); /* no thread join on PuTTY GUI */
}
void ai_bridge_capture(AiBridge *b,const void *data,size_t size)
{
    if (b && (load(&b->flags)&F_READ) && ai_ring_push(&b->ring,data,size)) SetEvent(b->wake);
}
static void revoke_control(AiBridge *b,int human)
{
    if (!b) return;
    InterlockedAnd(&b->flags,~AI_CONTROL_MASK);
    if (human) InterlockedIncrement64(&b->epoch);
    b->lease_until=0;
    b->command_until=0;
    InterlockedCompareExchange(&b->inbox_state,5,4);
    if (b->approval) PostMessage(b->approval,WM_CLOSE,0,0);
    if (load(&b->inbox_state)==2) {
        b->ack=2; b->outgoing_ack=b->incoming; InterlockedExchange(&b->inbox_state,3);
    }
    SetEvent(b->wake);
}
void ai_bridge_human_input(AiBridge *b) { revoke_control(b,1); }
void ai_bridge_add_menu(HMENU m)
{
    AppendMenuA(m,MF_SEPARATOR,0,NULL);
    AppendMenuA(m,MF_ENABLED,IDM_AI_OBSERVE,"AI OFF - enable observation...");
    AppendMenuA(m,MF_GRAYED,IDM_AI_CONTROL,"AI: approve pending control...");
    AppendMenuA(m,MF_GRAYED,IDM_AI_COMMAND,"AI: approve pending command...");
    AppendMenuA(m,MF_GRAYED,IDM_AI_REVOKE,"AI: revoke control");
}
static void update_menu(AiBridge *b,HMENU m)
{
    LONG flags=b?load(&b->flags):0;
    int pending=b && load(&b->inbox_state)==2;
    ModifyMenuA(m,IDM_AI_OBSERVE,MF_BYCOMMAND|MF_STRING,IDM_AI_OBSERVE,
        !b?"AI OFF - enable observation...":flags&F_CUSTOM_CONTROL?"AI CUSTOM SHELL AUTHORIZED - disable AI":flags&F_CONTROL?"AI DIAGNOSTIC AUTHORIZED - disable AI":"AI READ ONLY - disable AI");
    EnableMenuItem(m,IDM_AI_CONTROL,MF_BYCOMMAND|(pending && (b->incoming.kind==10 || b->incoming.kind==13)?MF_ENABLED:MF_GRAYED));
    EnableMenuItem(m,IDM_AI_COMMAND,MF_BYCOMMAND|(pending && (b->incoming.kind==11 || b->incoming.kind==14)?MF_ENABLED:MF_GRAYED));
    EnableMenuItem(m,IDM_AI_REVOKE,MF_BYCOMMAND|(b?MF_ENABLED:MF_GRAYED));
    if (!pending) {
        ModifyMenuA(m,IDM_AI_CONTROL,MF_BYCOMMAND|MF_STRING|MF_GRAYED,IDM_AI_CONTROL,
                    b && b->approval?"AI: approval window is already open":"AI: no pending control request");
        ModifyMenuA(m,IDM_AI_COMMAND,MF_BYCOMMAND|MF_STRING|MF_GRAYED,IDM_AI_COMMAND,
                    b && b->approval?"AI: approval window is already open":"AI: no pending command");
    }
    if (pending && (b->incoming.kind==10 || b->incoming.kind==13)) {
        char label[128];
        snprintf(label,sizeof(label),"AI: approve %s from client %02x%02x%02x%02x...",b->incoming.kind==13?"CUSTOM SHELL":"diagnostic",
                 b->payload[0],b->payload[1],b->payload[2],b->payload[3]);
        ModifyMenuA(m,IDM_AI_CONTROL,MF_BYCOMMAND|MF_STRING|MF_ENABLED,IDM_AI_CONTROL,label);
    }
}
void ai_bridge_dispatch(AiBridge *b,HWND hwnd,HMENU context)
{
    if (b) {
        if (InterlockedExchange(&b->revoke,0) ||
            ((load(&b->flags)&F_CONTROL) && GetTickCount64()>=b->lease_until)) revoke_control(b,0);
        if (load(&b->inbox_state)==2 && !b->command_until) {
            DWORD timeout=60000;
            if (b->incoming.kind==11) memcpy(&timeout,b->payload+20,4);
            if (b->incoming.kind==14) memcpy(&timeout,b->payload+16,4);
            b->command_until=GetTickCount64()+timeout;
        }
        if ((load(&b->inbox_state)==2 || load(&b->inbox_state)==4) &&
            b->command_until && GetTickCount64()>=b->command_until) revoke_control(b,0);
    }
    update_menu(b,GetSystemMenu(hwnd,FALSE)); if (context) update_menu(b,context);
}
int ai_bridge_is_menu(UINT id) { return id>=IDM_AI_OBSERVE && id<=IDM_AI_REVOKE && !(id&15); }
int ai_bridge_access_state(AiBridge *b)
{
    if (!b) return 0;
    if (!load(&b->online)) return 3;
    return load(&b->flags)&F_CUSTOM_CONTROL?4:load(&b->flags)&F_CONTROL?2:1;
}
void ai_bridge_title_prefix(AiBridge *b,wchar_t *out,size_t cap)
{
    int state=ai_bridge_access_state(b);
    if (!state) { if (cap) *out=0; return; }
    swprintf_s(out,cap,L"[AI %s %02x%02x%02x%02x] ",state==4?L"CUSTOM SHELL":state==2?L"DIAGNOSTIC":state==3?L"OFFLINE":L"READ ONLY",
               b->session[0],b->session[1],b->session[2],b->session[3]);
}
static const char *commands[]={"uname -a","uptime","df -h","free -m","iostat -x 1 5"};
typedef struct AiConsent {
    AiBridge **owner, *bridge;
    HWND hwnd;
    HMENU menu;
    UINT action;
    uint64_t expected_epoch;
    int (*send)(void *,const char *,size_t);
    void *context;
} AiConsent;
static void consent_complete(void *context,int approved)
{
    AiConsent *c=context; AiBridge *b=c->bridge;
    int live=IsWindow(c->hwnd);
    if (b) b->approval=NULL;
    if (c->action==IDM_AI_OBSERVE) {
        if (approved && live && !*c->owner && c->send(c->context,NULL,0)) *c->owner=start(c->hwnd);
    } else if (b) {
        b->ack=2;
        if (approved && live && *c->owner==b && load(&b->online) && !load(&b->revoke) &&
            load(&b->inbox_state)==4 && c->expected_epoch==epoch(b) &&
            GetTickCount64()<b->command_until && c->send(c->context,NULL,0)) {
            if (c->action==IDM_AI_CONTROL && (b->incoming.kind==10 || b->incoming.kind==13)) {
                memcpy(b->client,b->payload,16); b->lease_until=GetTickCount64()+60000;
                InterlockedAnd(&b->flags,~AI_CONTROL_MASK);
                InterlockedOr(&b->flags,F_CONTROL|(b->incoming.kind==13?F_CUSTOM_CONTROL:0)); b->ack=1;
            } else if (c->action==IDM_AI_COMMAND && (b->incoming.kind==11 || b->incoming.kind==14) &&
                       (load(&b->flags)&F_CONTROL) && !memcmp(b->client,b->payload,16) &&
                       GetTickCount64()<b->lease_until) {
                unsigned index=5; size_t i; char line[AI_INPUT_LINE_MAX],token[33];
                const char *command=NULL;
                if (b->incoming.kind==11 && !(load(&b->flags)&F_CUSTOM_CONTROL)) {
                    memcpy(&index,b->payload+16,4); if (index<5) command=commands[index];
                } else if (b->incoming.kind==14 && (load(&b->flags)&F_CUSTOM_CONTROL) &&
                           ai_command_validate((char *)b->payload+20,b->incoming.length-20)) command=(char *)b->payload+20;
                if (command) {
                    for(i=0;i<16;i++) snprintf(token+2*i,3,"%02x",b->incoming.request[i]);
                    if (ai_command_wrap(command,token,line,sizeof(line))) b->ack=c->send(c->context,line,strlen(line))?1:2;
                    SecureZeroMemory(line,sizeof(line));
                }
            }
        }
        b->command_until=0; b->outgoing_ack=b->incoming;
        InterlockedExchange(&b->inbox_state,3); SetEvent(b->wake);
    }
    if (live) { ai_bridge_dispatch(*c->owner,c->hwnd,c->menu); PostMessage(c->hwnd,WM_AI_BRIDGE,0,0); }
    if (b) release(b);
    HeapFree(GetProcessHeap(),0,c);
}
void ai_bridge_menu(AiBridge **owner,HWND hwnd,HMENU menu,UINT action,int ready,
                    int (*send)(void *,const char *,size_t),void *ctx)
{
    AiBridge *b=*owner; AiConsent *c; HWND prompt;
    char text[AI_COMMAND_MAX+1800]; const char *title="PuTTY AI authorization";
    if (action==IDM_AI_OBSERVE && b) { ai_bridge_stop(owner); ai_bridge_dispatch(NULL,hwnd,menu); return; }
    if (action==IDM_AI_REVOKE) { ai_bridge_human_input(b); ai_bridge_dispatch(b,hwnd,menu); return; }
    if (!ready || (b && (!load(&b->online) || load(&b->revoke) || b->approval))) return;
    if (action==IDM_AI_OBSERVE) {
        snprintf(text,sizeof(text),"Allow local Codex clients to read NEW terminal output in this window?\n\n"
            "Confirm authentication and MFA have finished and this is the intended target.\n\n"
            "Output may be sent to your configured AI provider. Redaction is best effort.\n"
            "Do not enable on a session containing secrets. No history or credentials are read.\n\n"
            "This window is modeless: terminal processing continues while you decide.");
    } else {
        if (!b || load(&b->inbox_state)!=2 || ((action!=IDM_AI_CONTROL || (b->incoming.kind!=10 && b->incoming.kind!=13)) &&
                   (action!=IDM_AI_COMMAND || (b->incoming.kind!=11 && b->incoming.kind!=14))) ||
            InterlockedCompareExchange(&b->inbox_state,4,2)!=2) return;
        if (action==IDM_AI_CONTROL) {
            snprintf(text,sizeof(text),"Allow client %02x%02x%02x%02x... %s control of this PuTTY window for up to 60 seconds?\n\n"
                "%s\n\nEach command still requires confirmation in THIS window.\n\n"
                "Keyboard, terminal mouse input or paste immediately revoke control.\n\n"
                "The terminal continues processing while this modeless window is open.",
                b->payload[0],b->payload[1],b->payload[2],b->payload[3],
                b->incoming.kind==13?"CUSTOM SHELL":"diagnostic",
                b->incoming.kind==13?"Custom commands may change files, install software, deploy services or modify the system. Approve only the client and target you intend to control.":"Diagnostic mode permits only five fixed read-only templates.");
        } else {
            unsigned index=5; const char *command=NULL;
            if (b->incoming.kind==11) { memcpy(&index,b->payload+16,4); if(index<5) command=commands[index]; }
            else if ((load(&b->flags)&F_CUSTOM_CONTROL) && ai_command_validate((char *)b->payload+20,b->incoming.length-20)) command=(char *)b->payload+20;
            if (!command) { InterlockedExchange(&b->inbox_state,2); ai_bridge_human_input(b); return; }
            snprintf(text,sizeof(text),"MODE: %s\nConfirm this window is the intended Linux target, at an IDLE POSIX shell with an EMPTY input line.\n\n"
                "Do not approve at a password/sudo/MFA prompt, or in vim/top/less/another program.\n\n"
                "If unsure, reject. Typing in the terminal revokes approval. Custom text can modify or deploy the system.\n"
                "The complete approved script follows (scroll to inspect all lines):\n\n--- BEGIN SCRIPT ---\n%s\n--- END SCRIPT ---",
                b->incoming.kind==14?"CUSTOM SHELL":"DIAGNOSTIC",command);
        }
    }
    c=HeapAlloc(GetProcessHeap(),HEAP_ZERO_MEMORY,sizeof(*c));
    if (!c) { if (b) InterlockedExchange(&b->inbox_state,2); return; }
    c->owner=owner; c->bridge=b; c->hwnd=hwnd; c->menu=menu; c->action=action;
    c->send=send; c->context=ctx; c->expected_epoch=b?epoch(b):0;
    if (b) InterlockedIncrement(&b->references);
    prompt=ai_approval_open(hwnd,title,text,consent_complete,c);
    if (!prompt) {
        if (b) { InterlockedExchange(&b->inbox_state,2); release(b); }
        HeapFree(GetProcessHeap(),0,c);
    } else if (b) b->approval=prompt;
    ai_bridge_dispatch(*owner,hwnd,menu);
}
void ai_bridge_offer_pending(AiBridge **owner,HWND hwnd,HMENU menu,int ready,
                            int (*send)(void *,const char *,size_t),void *ctx)
{
    AiBridge *b=*owner; UINT action;
    if (!b || !ready || !load(&b->online) || load(&b->revoke) ||
        b->approval || load(&b->inbox_state)!=2) return;
    action=(b->incoming.kind==10 || b->incoming.kind==13)?IDM_AI_CONTROL:IDM_AI_COMMAND;
    ai_bridge_menu(owner,hwnd,menu,action,ready,send,ctx);
}
