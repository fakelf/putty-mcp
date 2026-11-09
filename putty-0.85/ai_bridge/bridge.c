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
#define F_TOP 4
#define F_CHILD_MODE 8
#define F_CHILD_SESSION 16
#define F_AUTO_RUN 32
#define F_TERMINAL 64
#define AI_REQUEST_AUTO_RUN UINT32_C(0x80000000)
#define AI_APPROVAL_GENERATION_SHIFT 8
#define AI_APPROVAL_GENERATION_MASK UINT32_C(0x00ffffff)
#define AI_WIRE_VERSION 2
#define AI_TOP_EVENT 4
#define AI_TOP_SCREEN 5
#define AI_CHILD_EVENT 6
#define AI_TERMINAL_SCREEN 7
#define AI_COMMAND 11
#define AI_REVOKE 12
#define AI_INTERRUPT 13
#define AI_SEND_INTERRUPT 15
#define AI_TOP_START 16
#define AI_TOP_QUIT 17
#define AI_TOP_INTERRUPT 18
#define AI_TOP_SEND_KEY 19
#define AI_VI_START 20
#define AI_VI_QUIT 21
#define AI_SPECIAL_KEY 22
#define AI_VI_INSERT_TEXT 23
#define AI_VI_FILE_START 24
#define AI_VI_SAVE 25
#define AI_CHILD_OPEN 26
#define AI_CHILD_CLOSE 27
#define AI_CHILD_COMMIT 28
#define AI_TERMINAL_ATTACH 29
#define AI_TERMINAL_INPUT 30
#define AI_TERMINAL_COMMIT 31
#define AI_TERMINAL_DETACH 32
#define ACK_COMMAND_SENT 1
#define ACK_COMMAND_NOT_SENT 2
#define ACK_INTERRUPT_APPROVED 3
#define ACK_INTERRUPT_NOT_SENT 4
#define ACK_INTERRUPT_SENT 5
#define ACK_TOP_STARTED 6
#define ACK_TOP_NOT_STARTED 7
#define ACK_TOP_KEY_APPROVED 8
#define ACK_TOP_KEY_SENT 9
#define ACK_TOP_KEY_NOT_SENT 10
#define ACK_CHILD_APPROVED 11
#define ACK_CHILD_ACCEPTED 12
#define ACK_CHILD_REJECTED 13
#define ACK_TERMINAL_ATTACHED 14
#define ACK_TERMINAL_ATTACH_NOT_SENT 15
#define ACK_TERMINAL_INPUT_APPROVED 16
#define ACK_TERMINAL_INPUT_SENT 17
#define ACK_TERMINAL_INPUT_NOT_SENT 18
#define ACK_TERMINAL_ATTACH_APPROVED 19
#define ACK_TERMINAL_DETACHED 20
#define ACK_TERMINAL_DETACH_NOT_SENT 21
#define AI_SOURCE_MAX 128
#define AI_COMMAND_PREFIX 17 /* Client UUID and one-byte source length. */
#define AI_SCREEN_PREFIX 3 /* Flags, zero-based cursor column, cursor row. */
#define AI_TERMINAL_SCREEN_PREFIX 9 /* Flags, size, and cursor in little endian. */
#define AI_TERMINAL_INPUT_PREFIX 24 /* Task UUID and optional screen revision. */
#define AI_CHILD_EVENTS 8
enum { INBOX_IDLE, INBOX_FILLING, INBOX_WAITING, INBOX_ACK, INBOX_OPEN,
       INBOX_INVALID, INBOX_SENDING, INBOX_INTERRUPT_ALLOWED, INBOX_INTERRUPT_SEND };
typedef struct AiHeader {
    char magic[4]; uint16_t version, kind; uint32_t length, flags;
    unsigned char session[16], request[16]; uint64_t sequence, epoch;
} AiHeader;
typedef char header_must_be_64[(sizeof(AiHeader)==64)?1:-1];
typedef struct AiTaskPayload {
    AiTaskProgram program;
    size_t path_offset, path_length, text_offset;
} AiTaskPayload;
typedef struct AiChildEvent {
    unsigned char id[16];
    uint32_t stage;
} AiChildEvent;
struct AiBridge {
    AiRing ring;
    HANDLE stop, wake;
    HWND hwnd;
    volatile LONG references, flags, online, inbox_state;
    volatile LONG close_notification;
    volatile LONG approval_generation;
    volatile LONG64 epoch;
    unsigned char session[16];
    unsigned char parent[16];
    int is_child;
    unsigned char last_sent[16]; /* GUI-owned; only this task can be interrupted. */
    uint64_t last_sent_epoch;
    AiHeader incoming;
    LONG incoming_approval_generation;
    int incoming_auto_run;
    AiTaskPayload task;
    unsigned char payload[AI_COMMAND_PREFIX+AI_SOURCE_MAX+AI_COMMAND_MAX+1];
    DWORD ack;
    uint64_t command_offset;
    size_t command_prompt_len;
    char command_prompt[AI_COMMAND_PROMPT_MAX];
    uint64_t interrupt_offset;
    unsigned char terminal_id[16]; /* GUI-owned, scoped to terminal_epoch. */
    uint64_t terminal_epoch, terminal_offset;
    int terminal_bound;
    SRWLOCK terminal_lock;
    uint64_t terminal_frame, terminal_screen_epoch;
    unsigned char terminal_screen_task[16];
    size_t terminal_screen_len;
    unsigned char terminal_screen[AI_TERMINAL_SCREEN_PREFIX + AI_TERMINAL_SCREEN_MAX];
    SRWLOCK top_lock;
    unsigned char top_id[16];
    uint64_t top_event_revision, top_screen_revision, top_frame;
    uint32_t top_status[3];
    unsigned char top_screen[AI_SCREEN_PREFIX + AI_TOP_SCREEN_MAX];
    size_t top_screen_len;
    AiChildEvent child_events[AI_CHILD_EVENTS]; /* Protected by top_lock. */
    unsigned child_read, child_count;
    char top_target[384]; /* GUI-only; from the authenticated SSH backend */
    HWND approval; /* GUI only; modeless, never holds the PuTTY event loop */
};
static LONG load(volatile LONG *p) { return InterlockedCompareExchange(p,0,0); }
static uint64_t epoch(AiBridge *b) { return (uint64_t)InterlockedCompareExchange64(&b->epoch,0,0); }
static uint32_t approval_generation(AiBridge *b)
{
    return (uint32_t)load(&b->approval_generation)&AI_APPROVAL_GENERATION_MASK;
}
static void invalidate_pending(AiBridge *b)
{
    LONG state;
    do {
        state=load(&b->inbox_state);
        if (state!=INBOX_WAITING && state!=INBOX_OPEN &&
            state!=INBOX_INTERRUPT_ALLOWED && state!=INBOX_INTERRUPT_SEND) return;
    } while (InterlockedCompareExchange(&b->inbox_state,INBOX_INVALID,state)!=state);
}
static void finish_request(AiBridge *b,DWORD ack)
{
    b->ack=ack;
    InterlockedExchange(&b->inbox_state,INBOX_ACK);
    SetEvent(b->wake);
}
static DWORD not_sent(const AiBridge *b)
{
    switch (b->incoming.kind) {
      case AI_INTERRUPT: return ACK_INTERRUPT_NOT_SENT;
      case AI_TOP_START: case AI_VI_START: case AI_VI_FILE_START: return ACK_TOP_NOT_STARTED;
      case AI_TOP_QUIT: case AI_VI_QUIT: case AI_TOP_INTERRUPT:
      case AI_SPECIAL_KEY: case AI_VI_INSERT_TEXT: case AI_VI_SAVE: return ACK_TOP_KEY_NOT_SENT;
      case AI_CHILD_OPEN: case AI_CHILD_CLOSE: return ACK_CHILD_REJECTED;
      case AI_TERMINAL_ATTACH: return ACK_TERMINAL_ATTACH_NOT_SENT;
      case AI_TERMINAL_INPUT: return ACK_TERMINAL_INPUT_NOT_SENT;
      case AI_TERMINAL_DETACH: return ACK_TERMINAL_DETACH_NOT_SENT;
      default: return ACK_COMMAND_NOT_SENT;
    }
}
const char *ai_task_command(AiTaskProgram program)
{
    switch (program) {
      case AI_PROGRAM_TOP: return "exec top -d 2";
      case AI_PROGRAM_VI:
        return "exec vi -Nu NONE -i NONE -n -R -Z -c 'set encoding=utf-8 backspace=indent,eol,start'";
      default: return NULL;
    }
}
int ai_task_is_vi(AiTaskProgram program)
{
    return program==AI_PROGRAM_VI || program==AI_PROGRAM_VI_FILE;
}
const AiSpecialKey *ai_task_special_key(AiInputKind kind)
{
    /* The dedicated PTY advertises TERM=xterm. The order is the wire enum. */
    static const AiSpecialKey keys[] = {
        {"Escape", "\033"}, {"F1", "\033OP"}, {"F2", "\033OQ"},
        {"F3", "\033OR"}, {"F4", "\033OS"}, {"F5", "\033[15~"},
        {"F6", "\033[17~"}, {"F7", "\033[18~"}, {"F8", "\033[19~"},
        {"F9", "\033[20~"}, {"F10", "\033[21~"}, {"F11", "\033[23~"},
        {"F12", "\033[24~"}, {"Ctrl+Q", "\021"}, {"Ctrl+S", "\023"},
        {"Ctrl+D", "\004"}, {"Ctrl+Z", "\032"},
        {"Backspace", "\177"}, {"Delete", "\033[3~"}, {"Up", "\033[A"},
        {"Down", "\033[B"}, {"Left", "\033[D"}, {"Right", "\033[C"},
        {"Home", "\033[H"}, {"End", "\033[F"}, {"Page Up", "\033[5~"},
        {"Page Down", "\033[6~"}, {"Insert", "\033[2~"}, {"Tab", "\t"},
        {"Shift+Tab", "\033[Z"}, {"Enter", "\r"}, {"Undo", "\033u"},
        {"Redo", "\033\022"}, {"Enter insert mode", "\033i"},
        {"Discard and quit", "\033:qa!\r"},
    };
    if (kind < AI_INPUT_KEY_ESCAPE || kind > AI_INPUT_KEY_DISCARD_AND_QUIT) return NULL;
    return &keys[kind - AI_INPUT_KEY_ESCAPE];
}
static int task_start(unsigned kind)
{
    return kind==AI_TOP_START || kind==AI_VI_START || kind==AI_VI_FILE_START;
}
static int task_key(unsigned kind)
{
    return kind==AI_TOP_QUIT || kind==AI_VI_QUIT || kind==AI_TOP_INTERRUPT ||
        kind==AI_SPECIAL_KEY || kind==AI_VI_INSERT_TEXT || kind==AI_VI_SAVE;
}
static int child_request(unsigned kind)
{
    return kind==AI_CHILD_OPEN || kind==AI_CHILD_CLOSE;
}
static int terminal_request(unsigned kind)
{
    return kind==AI_TERMINAL_ATTACH || kind==AI_TERMINAL_INPUT || kind==AI_TERMINAL_DETACH;
}
static size_t terminal_payload_offset(const AiBridge *b)
{
    return AI_COMMAND_PREFIX+b->payload[16];
}
static AiInputKind input_kind(const AiHeader *request)
{
    switch (request->kind) {
      case AI_INTERRUPT: return AI_INPUT_INTERRUPT;
      case AI_TOP_START: case AI_VI_START: case AI_VI_FILE_START: return AI_INPUT_TASK_START;
      case AI_TOP_QUIT: case AI_VI_QUIT: return AI_INPUT_TASK_QUIT;
      case AI_TOP_INTERRUPT: return AI_INPUT_TASK_INTERRUPT;
      case AI_VI_INSERT_TEXT: return AI_INPUT_VI_TEXT;
      case AI_VI_SAVE: return AI_INPUT_VI_SAVE;
      case AI_CHILD_OPEN: return AI_INPUT_CHILD_OPEN;
      case AI_CHILD_CLOSE: return AI_INPUT_CHILD_CLOSE;
      case AI_TERMINAL_ATTACH: return AI_INPUT_ATTACH;
      case AI_TERMINAL_INPUT:
        return request->flags==1?AI_INPUT_KEY:
            request->flags==2?AI_INPUT_TEXT:AI_INPUT_PASTE;
      case AI_SPECIAL_KEY: return (AiInputKind)(AI_INPUT_KEY_ESCAPE + request->flags - 1);
      default: return AI_INPUT_SCRIPT;
    }
}
static int send_path_input(AiSend send,void *ctx,const AiBridge *b,AiInputKind kind)
{
    char input[16+AI_VI_PATH_MAX];
    memcpy(input,b->incoming.request,16);
    memcpy(input+16,b->payload+b->task.path_offset,b->task.path_length);
    return send(ctx,kind,b->task.program,input,16+b->task.path_length);
}
static int check_input(AiSend send, void *ctx, AiBridge *b)
{
    LONG flags=load(&b->flags);
    if (b->incoming_auto_run!=!!(flags&F_AUTO_RUN) ||
        b->incoming.sequence!=approval_generation(b) ||
        b->incoming_approval_generation!=load(&b->approval_generation)) return 0;
    if (terminal_request(b->incoming.kind)) {
        if (!(flags&F_TERMINAL)) return 0;
        if (b->incoming.kind==AI_TERMINAL_ATTACH)
            return send(ctx,AI_INPUT_ATTACH,AI_PROGRAM_NONE,NULL,0);
        const unsigned char *payload=b->payload+terminal_payload_offset(b);
        if (!ai_bridge_terminal_active(b) || memcmp(payload,b->terminal_id,16)) return 0;
        if (b->incoming.kind==AI_TERMINAL_DETACH) return 1;
        uint64_t frame;
        memcpy(&frame,payload+16,sizeof(frame));
        if (frame && (!send(ctx,AI_INPUT_SCREEN_REFRESH,AI_PROGRAM_NONE,NULL,0) ||
                      !b->terminal_screen_len || frame!=b->terminal_frame)) return 0;
        return send(ctx,input_kind(&b->incoming),AI_PROGRAM_NONE,NULL,0);
    }
    if (b->incoming.kind==AI_VI_SAVE)
        return send_path_input(send,ctx,b,AI_INPUT_VI_SAVE_CHECK);
    if (child_request(b->incoming.kind)) {
        LONG required=b->incoming.kind==AI_CHILD_OPEN?F_CHILD_MODE:F_CHILD_SESSION;
        if (!(flags&required)) return 0;
        return send(ctx,input_kind(&b->incoming),AI_PROGRAM_NONE,
                    (const char *)b->incoming.request,0);
    }
    return send(ctx,input_kind(&b->incoming),b->task.program,
                b->task.program?(const char *)b->incoming.request:NULL,0);
}
static AiTaskProgram task_program(const char *command,size_t length)
{
    for (AiTaskProgram program=AI_PROGRAM_TOP;program<=AI_PROGRAM_VI;program++) {
        const char *expected=ai_task_command(program);
        if (length==strlen(expected) && !memcmp(command,expected,length)) return program;
    }
    return AI_PROGRAM_NONE;
}
static int parse_task_payload(AiTaskPayload *task,const AiHeader *request,
                              const unsigned char *data,size_t offset)
{
    static const char file_tag[]="vi-file";
    size_t length=request->length;
    unsigned kind=request->kind;
    if (length-offset>=sizeof(file_tag)+2 && !memcmp(data+offset,file_tag,sizeof(file_tag))) {
        offset+=sizeof(file_tag);
        size_t path_length=data[offset]|((size_t)data[offset+1]<<8);
        offset+=2;
        if (path_length>length-offset ||
            !ai_file_path_validate((const char *)data+offset,path_length)) return 0;
        task->program=AI_PROGRAM_VI_FILE;
        task->path_offset=offset;
        task->path_length=path_length;
        offset+=path_length;
    } else if (kind==AI_VI_INSERT_TEXT) {
        const char *command=ai_task_command(AI_PROGRAM_VI);
        size_t command_length=strlen(command);
        if (length-offset<=command_length || memcmp(data+offset,command,command_length)) return 0;
        task->program=AI_PROGRAM_VI;
        offset+=command_length;
    } else {
        task->program=task_program((const char *)data+offset,length-offset);
        offset=length;
    }
    if (kind==AI_VI_INSERT_TEXT) {
        if (length-offset>AI_VI_TEXT_MAX ||
            !ai_text_validate((const char *)data+offset,length-offset)) return 0;
        task->text_offset=offset;
    } else if (offset!=length) {
        return 0;
    }
    AiTaskProgram program=task->program;
    if (!program ||
        ((kind==AI_TOP_START || kind==AI_TOP_QUIT) && program!=AI_PROGRAM_TOP) ||
        (kind==AI_VI_START && program!=AI_PROGRAM_VI) ||
        ((kind==AI_VI_FILE_START || kind==AI_VI_SAVE) && program!=AI_PROGRAM_VI_FILE) ||
        (kind==AI_VI_QUIT && !ai_task_is_vi(program))) return 0;
    if (kind!=AI_SPECIAL_KEY) return request->flags==0;
    return request->flags && request->flags<=AI_INPUT_KEY_DISCARD_AND_QUIT-AI_INPUT_KEY_ESCAPE+1 &&
        (request->flags<AI_INPUT_KEY_UNDO-AI_INPUT_KEY_ESCAPE+1 || ai_task_is_vi(program));
}
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
        ok=swprintf_s(name,cap,L"\\\\.\\pipe\\putty-ai-v440-%s",sid)>0;
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
    memset(h,0,sizeof(*h)); memcpy(h->magic,"PAI1",4); h->version=AI_WIRE_VERSION;
    h->kind=(uint16_t)kind; h->length=length;
    h->flags=(uint32_t)load(&b->flags)|(approval_generation(b)<<AI_APPROVAL_GENERATION_SHIFT);
    memcpy(h->session,b->session,16); h->epoch=epoch(b);
}
static int commit_matches(unsigned kind,unsigned request)
{
    switch (kind) {
      case AI_SEND_INTERRUPT: return request==AI_INTERRUPT;
      case AI_TOP_SEND_KEY: return task_key(request);
      case AI_CHILD_COMMIT: return child_request(request);
      case AI_TERMINAL_COMMIT: return request==AI_TERMINAL_ATTACH || request==AI_TERMINAL_INPUT;
      default: return 0;
    }
}
static int valid_terminal_payload(const AiHeader *request,const unsigned char *payload,size_t length)
{
    if (request->kind==AI_TERMINAL_ATTACH) return !request->flags && !length;
    if (request->kind==AI_TERMINAL_DETACH) return !request->flags && length==16;
    if (length<=AI_TERMINAL_INPUT_PREFIX || request->flags<1 || request->flags>3) return 0;
    const char *input=(const char *)payload+AI_TERMINAL_INPUT_PREFIX;
    length-=AI_TERMINAL_INPUT_PREFIX;
    if (request->flags==1) {
        if (length>AI_TERMINAL_KEY_MAX) return 0;
        for (size_t i=0;i<length;i++)
            if (input[i]<'!' || input[i]>'~') return 0;
        return 1;
    }
    if (request->flags==2 && (memchr(input,'\n',length) || memchr(input,'\t',length))) return 0;
    return length<=AI_TERMINAL_TEXT_MAX && ai_text_validate(input,length);
}
static int exchange(AiBridge *b,HANDLE pipe,AiHeader *h,void *payload)
{
    AiHeader reply; unsigned char data[sizeof(b->payload)];
    if (!transfer(b,pipe,h,sizeof(*h),1) ||
        (h->length && !transfer(b,pipe,payload,h->length,1)) ||
        !transfer(b,pipe,&reply,sizeof(reply),0)) return 0;
    if (memcmp(reply.magic,"PAI1",4) || reply.version!=AI_WIRE_VERSION || reply.length>=sizeof(data) ||
        memcmp(reply.session,b->session,16)) return 0;
    if (reply.length && !transfer(b,pipe,data,reply.length,0)) return 0;
    if (reply.kind==AI_REVOKE || reply.kind==AI_SEND_INTERRUPT ||
        reply.kind==AI_TOP_SEND_KEY || reply.kind==AI_CHILD_COMMIT ||
        reply.kind==AI_TERMINAL_COMMIT) {
        LONG state=load(&b->inbox_state);
        if (reply.length) return 0;
        if (state!=INBOX_IDLE && !memcmp(reply.request,b->incoming.request,16)) {
            if (commit_matches(reply.kind,b->incoming.kind) &&
                state==INBOX_INTERRUPT_ALLOWED &&
                reply.epoch==epoch(b)) {
                InterlockedCompareExchange(&b->inbox_state,INBOX_INTERRUPT_SEND,INBOX_INTERRUPT_ALLOWED);
            } else {
                invalidate_pending(b);
            }
            PostMessage(b->hwnd,WM_AI_BRIDGE,0,0);
        }
    } else if (reply.kind==AI_COMMAND || reply.kind==AI_INTERRUPT ||
               task_start(reply.kind) || task_key(reply.kind) || child_request(reply.kind) ||
               terminal_request(reply.kind)) {
        unsigned source_len, command_offset;
        int auto_run=!!(reply.flags&AI_REQUEST_AUTO_RUN);
        reply.flags&=~AI_REQUEST_AUTO_RUN;
        AiTaskPayload task={0};
        const char *source=(const char *)data+AI_COMMAND_PREFIX;
        if (reply.length<=AI_COMMAND_PREFIX) return 0;
        source_len=data[16]; command_offset=AI_COMMAND_PREFIX+source_len;
        if (!source_len || source_len>AI_SOURCE_MAX || reply.length<command_offset ||
            memchr(source,'\n',source_len) || memchr(source,'\t',source_len) ||
            !ai_command_validate(source,source_len)) return 0;
        if (child_request(reply.kind)) {
            if (reply.length!=command_offset || reply.flags) return 0;
        } else if (terminal_request(reply.kind)) {
            if (!valid_terminal_payload(&reply,data+command_offset,reply.length-command_offset)) return 0;
        } else if (reply.length==command_offset) {
            return 0;
        } else if (task_start(reply.kind) || task_key(reply.kind)) {
            if (!parse_task_payload(&task,&reply,data,command_offset)) return 0;
        } else if (!ai_command_validate((const char *)data+command_offset,reply.length-command_offset)) {
            return 0;
        }
        if (InterlockedCompareExchange(&b->inbox_state,INBOX_FILLING,INBOX_IDLE)==INBOX_IDLE) {
            b->incoming=reply; b->task=task; memcpy(b->payload,data,reply.length);
            b->incoming_approval_generation=load(&b->approval_generation);
            b->incoming_auto_run=auto_run;
            b->payload[reply.length]=0;
            InterlockedExchange(&b->inbox_state,INBOX_WAITING);
            if (reply.epoch!=epoch(b)) invalidate_pending(b);
            PostMessage(b->hwnd,WM_AI_BRIDGE,0,0);
        } else return 0; /* fail closed; broker marks uncertain requests unknown */
    } else if (reply.kind!=0) return 0;
    return 1;
}
static int send_top_updates(AiBridge *b,HANDLE pipe,uint64_t *event,uint64_t *screen)
{
    AiHeader h;
    unsigned char bytes[AI_SCREEN_PREFIX + AI_TOP_SCREEN_MAX];
    uint32_t status[3];
    int event_changed, screen_changed;
    uint64_t frame;
    size_t length;
    AcquireSRWLockShared(&b->top_lock);
    event_changed=b->top_event_revision!=*event;
    length=b->top_screen_len;
    screen_changed=length && b->top_screen_revision!=*screen;
    header(b,&h,AI_TOP_EVENT,sizeof(status));
    memcpy(h.request,b->top_id,16);
    if (event_changed) memcpy(status,b->top_status,sizeof(status));
    if (screen_changed) memcpy(bytes,b->top_screen,length);
    frame=b->top_frame;
    *event=b->top_event_revision;
    *screen=b->top_screen_revision;
    ReleaseSRWLockShared(&b->top_lock);
    /* Send the final visible frame before announcing task closure. */
    if (screen_changed) {
        h.kind=AI_TOP_SCREEN; h.length=(uint32_t)length; h.sequence=frame;
        if (!exchange(b,pipe,&h,bytes)) return 0;
    }
    if (event_changed) {
        h.kind=AI_TOP_EVENT; h.length=sizeof(status); h.sequence=0;
        if (!exchange(b,pipe,&h,status)) return 0;
    }
    return 1;
}
static int send_terminal_update(AiBridge *b,HANDLE pipe,uint64_t *previous)
{
    AiHeader h;
    unsigned char bytes[AI_TERMINAL_SCREEN_PREFIX + AI_TERMINAL_SCREEN_MAX];
    AcquireSRWLockShared(&b->terminal_lock);
    uint64_t revision=b->terminal_frame;
    if (revision==*previous) {
        ReleaseSRWLockShared(&b->terminal_lock);
        return 1;
    }
    header(b,&h,AI_TERMINAL_SCREEN,0);
    if (b->terminal_screen_epoch==h.epoch) {
        h.length=(uint32_t)b->terminal_screen_len;
        if (h.length) memcpy(h.request,b->terminal_screen_task,16);
        memcpy(bytes,b->terminal_screen,h.length);
    }
    h.sequence=revision;
    *previous=revision;
    ReleaseSRWLockShared(&b->terminal_lock);
    return exchange(b,pipe,&h,bytes);
}
static int send_child_event(AiBridge *b,HANDLE pipe)
{
    AiChildEvent event;
    AcquireSRWLockShared(&b->top_lock);
    unsigned available=b->child_count;
    if (available) event=b->child_events[b->child_read];
    ReleaseSRWLockShared(&b->top_lock);
    if (!available) return 1;
    AiHeader h;
    header(b,&h,AI_CHILD_EVENT,sizeof(event.stage));
    memcpy(h.request,event.id,16);
    if (!exchange(b,pipe,&h,&event.stage)) return 0;
    AcquireSRWLockExclusive(&b->top_lock);
    b->child_read=(b->child_read+1)%AI_CHILD_EVENTS;
    --b->child_count;
    ReleaseSRWLockExclusive(&b->top_lock);
    return 1;
}
static DWORD WINAPI worker(void *ctx)
{
    AiBridge *b=ctx; wchar_t name[256]; TOKEN_USER *user=NULL;
    HANDLE pipe=INVALID_HANDLE_VALUE, events[2]={b->stop,b->wake}; DWORD backoff=250;
    AiHeader h; AiChunk chunk; unsigned char hello[36], status[20+AI_COMMAND_PROMPT_MAX];
    FILETIME c,e,k,u; DWORD pid=GetCurrentProcessId(); uint64_t start,hwnd=(uint64_t)(uintptr_t)b->hwnd;
    int child_registered=0;
    if (!own_pipe(name,256,&user)) goto done;
    if (!GetProcessTimes(GetCurrentProcess(),&c,&e,&k,&u)) goto done;
    start=((uint64_t)c.dwHighDateTime<<32)|c.dwLowDateTime;
    memcpy(hello,&pid,4); memcpy(hello+4,&start,8); memcpy(hello+12,&hwnd,8);
    if (b->is_child) memcpy(hello+20,b->parent,16);
    while (WaitForSingleObject(b->stop,0)!=WAIT_OBJECT_0) {
        DWORD peer=0;
        uint64_t top_event=0, top_screen=0, terminal_screen=0;
        pipe=CreateFileW(name,GENERIC_READ|GENERIC_WRITE,0,NULL,OPEN_EXISTING,
                         FILE_FLAG_OVERLAPPED|SECURITY_SQOS_PRESENT|SECURITY_IDENTIFICATION,NULL);
        if (pipe==INVALID_HANDLE_VALUE || !GetNamedPipeServerProcessId(pipe,&peer) || !identity(peer,user->User.Sid)) goto offline;
        header(b,&h,1,b->is_child?sizeof(hello):20); h.sequence=0;
        if (!exchange(b,pipe,&h,hello)) goto offline;
        child_registered=b->is_child;
        InterlockedExchange(&b->online,1); backoff=250;
        while (WaitForSingleObject(b->stop,0)!=WAIT_OBJECT_0) {
            unsigned batch=0;
            while (batch++<16 && ai_ring_pop(&b->ring,&chunk)) {
                header(b,&h,2,(unsigned)chunk.length); h.sequence=chunk.sequence;
                if (!exchange(b,pipe,&h,chunk.data)) goto offline;
            }
            header(b,&h,3,12); h.sequence=ai_ring_drained_sequence(&b->ring);
            memset(status,0,sizeof(status));
            int close_child=0;
            if (load(&b->inbox_state)==INBOX_ACK) {
                memcpy(h.request,b->incoming.request,16);
                memcpy(status,&b->ack,4);
                if (b->ack==ACK_COMMAND_SENT) {
                    memcpy(status+12,&b->command_offset,8);
                    memcpy(status+20,b->command_prompt,b->command_prompt_len);
                    h.length=20+(uint32_t)b->command_prompt_len;
                } else if (b->ack==ACK_INTERRUPT_SENT) {
                    memcpy(status+12,&b->interrupt_offset,8);
                    h.length=20;
                } else if (b->ack==ACK_TERMINAL_ATTACHED) {
                    memcpy(status+12,&b->terminal_offset,8);
                    h.length=20;
                }
                close_child=b->incoming.kind==AI_CHILD_CLOSE && b->ack==ACK_CHILD_ACCEPTED;
                InterlockedExchange(&b->inbox_state,
                    b->ack==ACK_INTERRUPT_APPROVED || b->ack==ACK_TOP_KEY_APPROVED ||
                    b->ack==ACK_CHILD_APPROVED || b->ack==ACK_TERMINAL_ATTACH_APPROVED ||
                    b->ack==ACK_TERMINAL_INPUT_APPROVED?
                    INBOX_INTERRUPT_ALLOWED:INBOX_IDLE);
            }
            { LONG64 dropped=InterlockedCompareExchange64(&b->ring.dropped,0,0); memcpy(status+4,&dropped,8); }
            int exchanged=exchange(b,pipe,&h,status);
            if (close_child) {
                InterlockedExchange(&b->close_notification,1);
                PostMessage(b->hwnd,WM_AI_BRIDGE,0,0);
            }
            if (!exchanged) goto offline;
            if (!send_top_updates(b,pipe,&top_event,&top_screen)) goto offline;
            if (!send_terminal_update(b,pipe,&terminal_screen)) goto offline;
            if (!send_child_event(b,pipe)) goto offline;
            if (InterlockedCompareExchange64(&b->ring.written,0,0)!=InterlockedCompareExchange64(&b->ring.consumed,0,0)) continue;
            if (WaitForMultipleObjects(2,events,FALSE,250)==WAIT_OBJECT_0) break;
        }
      offline:
        if (InterlockedExchange(&b->online,0)) InterlockedIncrement64(&b->epoch);
        invalidate_pending(b); PostMessage(b->hwnd,WM_AI_BRIDGE,0,0);
        if (pipe!=INVALID_HANDLE_VALUE) { CancelIoEx(pipe,NULL); CloseHandle(pipe); pipe=INVALID_HANDLE_VALUE; }
        if (child_registered) break;
        if (WaitForSingleObject(b->stop,backoff)==WAIT_OBJECT_0) break;
        if (backoff<5000) backoff=backoff*2>5000?5000:backoff*2;
    }
  done:
    if (pipe!=INVALID_HANDLE_VALUE) CloseHandle(pipe);
    if (user) HeapFree(GetProcessHeap(),0,user);
    release(b); return 0;
}
static AiBridge *start_bridge(HWND hwnd,const AiBridge *parent,const unsigned char *id)
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
        (!parent && BCryptGenRandom(NULL,b->session,16,BCRYPT_USE_SYSTEM_PREFERRED_RNG)!=0)) {
        if (b->ring.chunks) ai_ring_free(&b->ring);
        if (b->stop) CloseHandle(b->stop); if (b->wake) CloseHandle(b->wake);
        _aligned_free(b); return NULL;
    }
    b->hwnd=hwnd; b->flags=F_READ|F_CONNECTED|F_TERMINAL; b->epoch=1; b->references=2;
    if (parent) {
        memcpy(b->session,id,16);
        memcpy(b->parent,parent->session,16);
        memcpy(b->top_target,parent->top_target,sizeof(b->top_target));
        b->is_child=1;
        b->flags|=F_CHILD_SESSION|(parent->flags&F_AUTO_RUN);
    }
    thread=CreateThread(NULL,0,worker,b,0,NULL);
    if (!thread) { b->references=1; release(b); return NULL; }
    CloseHandle(thread); SetTimer(hwnd,AI_TIMER,250,NULL); return b;
}
static AiBridge *start(HWND hwnd)
{
    return start_bridge(hwnd,NULL,NULL);
}
AiBridge *ai_bridge_start_child(HWND hwnd,const AiBridge *parent,const unsigned char id[16])
{
    return parent && !parent->is_child && id?start_bridge(hwnd,parent,id):NULL;
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
void ai_bridge_command_prompt(AiBridge *b,const char *prompt,size_t length)
{
    if (!b) return;
    b->command_prompt_len=0;
    if (prompt && length && length<=sizeof(b->command_prompt) &&
        !memchr(prompt,'\n',length) && !memchr(prompt,'\t',length) &&
        ai_text_validate(prompt,length)) {
        memcpy(b->command_prompt,prompt,length);
        b->command_prompt_len=length;
    }
}
int ai_bridge_terminal_active(AiBridge *b)
{
    return b && b->terminal_bound && load(&b->online) &&
        (load(&b->flags)&(F_READ|F_CONNECTED|F_TERMINAL))==(F_READ|F_CONNECTED|F_TERMINAL) &&
        b->terminal_epoch==epoch(b);
}
void ai_bridge_terminal_screen(AiBridge *b,const char *text,size_t length,
                               int columns,int rows,int cursor_x,int cursor_y,unsigned flags)
{
    if (!b) return;
    unsigned char prefix[AI_TERMINAL_SCREEN_PREFIX];
    uint64_t current_epoch=epoch(b);
    size_t size=0;
    if (text) {
        if (!ai_bridge_terminal_active(b) || length>AI_TERMINAL_SCREEN_MAX ||
            columns<1 || rows<1 || columns>UINT16_MAX || rows>UINT16_MAX) return;
        int visible=cursor_x>=0 && cursor_x<columns && cursor_y>=0 && cursor_y<rows;
        uint16_t values[4]={(uint16_t)columns,(uint16_t)rows,
                            visible?(uint16_t)cursor_x:0,visible?(uint16_t)cursor_y:0};
        prefix[0]=(unsigned char)((flags&7)|(visible?8:0));
        memcpy(prefix+1,values,sizeof(values));
        size=sizeof(prefix)+length;
    }
    AcquireSRWLockExclusive(&b->terminal_lock);
    int changed=b->terminal_screen_epoch!=current_epoch || b->terminal_screen_len!=size ||
        (size && (memcmp(b->terminal_screen,prefix,sizeof(prefix)) ||
                  memcmp(b->terminal_screen+sizeof(prefix),text,length)));
    if (changed) {
        if (size) {
            memcpy(b->terminal_screen,prefix,sizeof(prefix));
            memcpy(b->terminal_screen+sizeof(prefix),text,length);
            memcpy(b->terminal_screen_task,b->terminal_id,16);
        } else if (b->terminal_screen_len) {
            SecureZeroMemory(b->terminal_screen,b->terminal_screen_len);
        }
        b->terminal_screen_len=size;
        b->terminal_screen_epoch=current_epoch;
        ++b->terminal_frame;
    }
    ReleaseSRWLockExclusive(&b->terminal_lock);
    if (changed) SetEvent(b->wake);
}
static void bind_terminal(AiBridge *b,const unsigned char task[16],uint64_t task_epoch)
{
    ai_bridge_terminal_screen(b,NULL,0,0,0,-1,-1,0);
    memcpy(b->terminal_id,task,16);
    b->terminal_epoch=task_epoch;
    b->terminal_bound=1;
}
static void unbind_terminal(AiBridge *b)
{
    b->terminal_bound=0;
    ai_bridge_terminal_screen(b,NULL,0,0,0,-1,-1,0);
}
void ai_bridge_human_input(AiBridge *b)
{
    if (!b) return;
    InterlockedIncrement64(&b->epoch);
    unbind_terminal(b);
    invalidate_pending(b);
    PostMessage(b->hwnd,WM_AI_BRIDGE,0,0);
    SetEvent(b->wake);
}
void ai_bridge_top_target(AiBridge *b,const char *target)
{
    if (!b) return;
    if (target && *target && strlen(target)<sizeof(b->top_target) &&
        !strchr(target,'\n') && !strchr(target,'\t') && ai_command_validate(target,strlen(target))) {
        strcpy_s(b->top_target,sizeof(b->top_target),target);
        if (!b->is_child) InterlockedOr(&b->flags,F_TOP);
    } else {
        b->top_target[0]=0;
        InterlockedAnd(&b->flags,~F_TOP);
    }
}
void ai_bridge_child_mode(AiBridge *b,int enabled)
{
    if (!b || b->is_child) return;
    const LONG mode=F_CHILD_MODE|F_AUTO_RUN;
    LONG previous=enabled?InterlockedOr(&b->flags,mode):InterlockedAnd(&b->flags,~mode);
    if ((!!(previous&F_CHILD_MODE))==!!enabled) return;
    InterlockedIncrement(&b->approval_generation);
    invalidate_pending(b);
    SetEvent(b->wake);
}
static void queue_child_event(AiBridge *b,const unsigned char id[16],unsigned stage)
{
    if (!b || b->is_child) return;
    AcquireSRWLockExclusive(&b->top_lock);
    if (b->child_count<AI_CHILD_EVENTS) {
        unsigned index=(b->child_read+b->child_count++)%AI_CHILD_EVENTS;
        memcpy(b->child_events[index].id,id,16);
        b->child_events[index].stage=stage;
    }
    ReleaseSRWLockExclusive(&b->top_lock);
    SetEvent(b->wake);
}
void ai_bridge_child_failed(AiBridge *b,const unsigned char id[16],unsigned stage)
{
    if (stage && stage<=4) queue_child_event(b,id,stage);
}
void ai_bridge_child_closed(AiBridge *b,const unsigned char id[16])
{
    queue_child_event(b,id,5);
}
void ai_bridge_top_state(AiBridge *b,const unsigned char *id,AiTopState state,int detail,int taken)
{
    if (!b) return;
    AcquireSRWLockExclusive(&b->top_lock);
    if (memcmp(b->top_id,id,16)) {
        memcpy(b->top_id,id,16);
        b->top_screen_len=0; b->top_frame=0;
    }
    b->top_status[0]=(uint32_t)state;
    b->top_status[1]=(uint32_t)detail;
    b->top_status[2]=(uint32_t)taken;
    ++b->top_event_revision;
    ReleaseSRWLockExclusive(&b->top_lock);
    LONG inbox=load(&b->inbox_state);
    if (state>=AI_TOP_EXITED && (inbox==INBOX_WAITING || inbox==INBOX_OPEN ||
        inbox==INBOX_INTERRUPT_ALLOWED || inbox==INBOX_INTERRUPT_SEND) &&
        task_key(b->incoming.kind) && !memcmp(id,b->incoming.request,16))
        invalidate_pending(b);
    SetEvent(b->wake);
}
void ai_bridge_top_screen(AiBridge *b,const unsigned char *id,const char *text,size_t len,
                          int truncated,int cursor_column,int cursor_row,int paste_available)
{
    if (!b || len>AI_TOP_SCREEN_MAX) return;
    AcquireSRWLockExclusive(&b->top_lock);
    if (!memcmp(b->top_id,id,16)) {
        int visible=cursor_column>=0 && cursor_column<240 && cursor_row>=0 && cursor_row<100;
        b->top_screen[0]=(unsigned char)((truncated?1:0)|(visible?2:0)|(paste_available?4:0));
        b->top_screen[1]=visible?(unsigned char)cursor_column:0;
        b->top_screen[2]=visible?(unsigned char)cursor_row:0;
        memcpy(b->top_screen+AI_SCREEN_PREFIX,text,len);
        b->top_screen_len=len+AI_SCREEN_PREFIX;
        ++b->top_frame;
        ++b->top_screen_revision;
    }
    ReleaseSRWLockExclusive(&b->top_lock);
    SetEvent(b->wake);
}
void ai_bridge_add_menu(HMENU m)
{
    AppendMenuA(m,MF_SEPARATOR,0,NULL);
    AppendMenuA(m,MF_ENABLED,IDM_AI_OBSERVE,"Enable AI...");
}
static void update_menu(AiBridge *b,HMENU m)
{
    ModifyMenuA(m,IDM_AI_OBSERVE,MF_BYCOMMAND|MF_STRING,IDM_AI_OBSERVE,
        b?"Disable AI":"Enable AI...");
}
void ai_bridge_dispatch(AiBridge *b,HWND hwnd,HMENU context)
{
    if (b && load(&b->inbox_state)==INBOX_INVALID) {
        if (b->approval) PostMessage(b->approval,WM_CLOSE,0,0);
        else finish_request(b,not_sent(b));
    }
    if (b && load(&b->close_notification) && PostMessage(hwnd,WM_AI_CLOSE_CHILD,0,0))
        InterlockedExchange(&b->close_notification,0);
    update_menu(b,GetSystemMenu(hwnd,FALSE)); if (context) update_menu(b,context);
}
int ai_bridge_access_state(AiBridge *b)
{
    if (!b) return 0;
    if (!load(&b->online)) return 3;
    return load(&b->flags)&F_AUTO_RUN?2:1;
}
void ai_bridge_title_prefix(AiBridge *b,wchar_t *out,size_t cap)
{
    int state=ai_bridge_access_state(b);
    if (!state) { if (cap) *out=0; return; }
    swprintf_s(out,cap,L"[AI %s%s %02x%02x%02x%02x] ",b->is_child?L"CHILD ":L"",
               state==3?L"OFFLINE":state==2?L"AUTO RUN":L"READ ONLY",
               b->session[0],b->session[1],b->session[2],b->session[3]);
}
typedef struct AiConsent {
    AiBridge **owner, *bridge;
    HWND hwnd;
    HMENU menu;
    int observation;
    uint64_t expected_epoch;
    AiSend send;
    void *context;
} AiConsent;
static int request_ready(AiBridge *b,AiSend send,void *ctx)
{
    return load(&b->online) && b->incoming.epoch==epoch(b) &&
        (b->incoming.kind!=AI_INTERRUPT ||
         (b->last_sent_epoch==b->incoming.epoch && !memcmp(b->last_sent,b->incoming.request,16))) &&
        check_input(send,ctx,b);
}

static DWORD authorize_request(AiBridge *b,AiSend send,void *ctx)
{
    unsigned request=b->incoming.kind;
    AiInputKind kind=input_kind(&b->incoming);
    /* The Broker still rechecks a committed action against the live task. */
    if (kind==AI_INPUT_INTERRUPT) return ACK_INTERRUPT_APPROVED;
    if (task_key(request)) return ACK_TOP_KEY_APPROVED;
    if (child_request(request)) return ACK_CHILD_APPROVED;
    if (request==AI_TERMINAL_ATTACH) return ACK_TERMINAL_ATTACH_APPROVED;
    if (request==AI_TERMINAL_INPUT) return ACK_TERMINAL_INPUT_APPROVED;
    if (request==AI_TERMINAL_DETACH) {
        unbind_terminal(b);
        return ACK_TERMINAL_DETACHED;
    }
    if (kind==AI_INPUT_TASK_START)
        return send_path_input(send,ctx,b,kind)?ACK_TOP_STARTED:ACK_TOP_NOT_STARTED;

    char line[AI_INPUT_LINE_MAX];
    size_t offset=AI_COMMAND_PREFIX+b->payload[16];
    size_t length=ai_command_input((const char *)b->payload+offset,
                                   b->incoming.length-offset,line,sizeof(line));
    b->command_offset=b->ring.sequence;
    b->command_prompt_len=0;
    int sent=length && send(ctx,kind,AI_PROGRAM_NONE,line,length);
    SecureZeroMemory(line,sizeof(line));
    if (!sent) return ACK_COMMAND_NOT_SENT;
    memcpy(b->last_sent,b->incoming.request,16);
    b->last_sent_epoch=b->incoming.epoch;
    bind_terminal(b,b->incoming.request,b->incoming.epoch);
    return ACK_COMMAND_SENT;
}
static void consent_complete(void *context,int approved)
{
    AiConsent *c=context; AiBridge *b=c->bridge;
    int live=IsWindow(c->hwnd);
    if (b) b->approval=NULL;
    if (c->observation) {
        if (approved && live && !*c->owner &&
            c->send(c->context,AI_INPUT_SCRIPT,AI_PROGRAM_NONE,NULL,0))
            *c->owner=start(c->hwnd);
    } else if (b) {
        DWORD ack=not_sent(b);
        if (approved && live && *c->owner==b && !b->incoming_auto_run &&
            c->expected_epoch==epoch(b) &&
            request_ready(b,c->send,c->context) &&
            InterlockedCompareExchange(&b->inbox_state,INBOX_SENDING,INBOX_OPEN)==INBOX_OPEN)
            ack=authorize_request(b,c->send,c->context);
        finish_request(b,ack);
    }
    if (live) { ai_bridge_dispatch(*c->owner,c->hwnd,c->menu); PostMessage(c->hwnd,WM_AI_BRIDGE,0,0); }
    if (b) release(b);
    HeapFree(GetProcessHeap(),0,c);
}
static void open_approval(AiBridge **owner,HWND hwnd,HMENU menu,int observation,AiSend send,void *ctx)
{
    AiBridge *b=*owner; AiConsent *c; HWND prompt;
    char text[AI_VI_FILE_COMMAND_MAX+AI_VI_PATH_MAX+AI_VI_TEXT_MAX+AI_SOURCE_MAX+1200];
    const char *title="PuTTY AI observation";
    char action_text[180], key_label[32];
    const char *approve_label="Approve";
    if (b && (!load(&b->online) || b->approval)) return;
    if (observation) {
        if (!send(ctx,AI_INPUT_SCRIPT,AI_PROGRAM_NONE,NULL,0)) return;
        snprintf(text,sizeof(text),"Allow local MCP clients to use this window and read terminal output?\n\n"
            "Confirm authentication and MFA have finished and this is the intended target.\n\n"
            "Normal mode asks for local approval before attaching, sending commands or typing. "
            "An active AI task can read the visible terminal screen. With AI multi-window mode checked, "
            "MCP can send input and manage child terminals without further local prompts.\n\n"
            "Output may be sent to your configured AI provider. Redaction is best effort.\n"
            "Clear sensitive text from the visible screen before enabling AI. Scrollback is excluded.\n\n"
            "This window is modeless: terminal processing continues while you decide.");
    } else {
        unsigned source_len;
        int interrupt;
        unsigned request_kind;
        const char *command;
        if (!b ||
            InterlockedCompareExchange(&b->inbox_state,INBOX_OPEN,INBOX_WAITING)!=INBOX_WAITING)
            return;
        request_kind=b->incoming.kind;
        interrupt=request_kind==AI_INTERRUPT;
        if (!request_ready(b,send,ctx)) {
            finish_request(b,not_sent(b));
            return;
        }
        source_len=b->payload[16];
        command=(const char *)b->payload+AI_COMMAND_PREFIX+source_len;
        if (child_request(request_kind)) {
            int opening=request_kind==AI_CHILD_OPEN;
            title=opening?"PuTTY AI - Open child terminal":"PuTTY AI - Close child terminal";
            approve_label=opening?"Open window":"Close window";
            snprintf(text,sizeof(text),"ACTION: %s\n\nSOURCE: %.*s\n"
                "CLIENT CONNECTION: %02x%02x%02x%02x\n"
                "TARGET: %s\nPuTTY session %02x%02x%02x%02x\n\n%s",
                opening?"Open an independent black child terminal on this SSH connection.":
                        "Close this child terminal and its SSH channel.",
                (int)source_len,(const char *)b->payload+AI_COMMAND_PREFIX,
                b->payload[0],b->payload[1],b->payload[2],b->payload[3],b->top_target,
                b->session[0],b->session[1],b->session[2],b->session[3],
                opening?"The child opens a new shell. Local MCP clients can read its output. "
                        "Commands still require separate approval.":
                        "Verify this child shell is idle before closing. The original PuTTY window stays connected.");
        } else if (request_kind==AI_TERMINAL_ATTACH || request_kind==AI_TERMINAL_INPUT) {
            int attach=request_kind==AI_TERMINAL_ATTACH;
            AiInputKind kind=input_kind(&b->incoming);
            const char *action=attach?"Attach to the active terminal and read its visible screen":
                kind==AI_INPUT_KEY?"Send one key combination":
                kind==AI_INPUT_TEXT?"Type one line of text (without Enter)":"Paste text using the terminal's paste mode";
            title=attach?"PuTTY AI - Attach to terminal":
                kind==AI_INPUT_KEY?"PuTTY AI - Send terminal key":
                kind==AI_INPUT_TEXT?"PuTTY AI - Type terminal text":"PuTTY AI - Paste terminal text";
            approve_label=attach?"Attach":kind==AI_INPUT_KEY?"Send key":
                kind==AI_INPUT_TEXT?"Type text":"Paste text";
            snprintf(text,sizeof(text),"ACTION: %s\n\nSOURCE: %.*s\n"
                "CLIENT CONNECTION: %02x%02x%02x%02x\n"
                "TARGET: PuTTY session %02x%02x%02x%02x (this window)\n%s\n\n"
                "Verify the current program and prompt before approving. Handle credentials manually.%s%s%s",
                action,(int)source_len,(const char *)b->payload+AI_COMMAND_PREFIX,
                b->payload[0],b->payload[1],b->payload[2],b->payload[3],
                b->session[0],b->session[1],b->session[2],b->session[3],b->top_target,
                attach?"":"\n\n--- BEGIN INPUT ---\n",
                attach?"":command+AI_TERMINAL_INPUT_PREFIX,
                attach?"":"\n--- END INPUT ---");
        } else if (b->task.program) {
            int vi=ai_task_is_vi(b->task.program);
            const char *program=vi?"vi":"top";
            const char *path=(const char *)b->payload+b->task.path_offset;
            const char *ssh_command=ai_task_command(b->task.program);
            char file_command[AI_VI_FILE_COMMAND_MAX];
            const char *description;
            if (request_kind==AI_VI_FILE_START) {
                if (!ai_task_file_command(path,b->task.path_length,file_command,sizeof(file_command))) {
                    finish_request(b,not_sent(b));
                    return;
                }
                ssh_command=file_command;
                title="PuTTY AI - Open remote file";
                approve_label="Open file";
                description="Open the approved file in vi. A separate approval is needed to save changes.";
            } else if (task_start(request_kind)) {
                title=vi?"PuTTY AI - Start vi":"PuTTY AI - Start top";
                approve_label=vi?"Start vi":"Start top";
                description=vi?"Open an empty vi buffer for scratch edits on a separate SSH task channel.":
                    "Run top in this PuTTY window on a separate SSH task channel.";
            } else if (request_kind==AI_VI_SAVE) {
                title="PuTTY AI - Save remote file";
                approve_label="Save file";
                description="Write the current vi buffer to the approved file once using a non-forced save.";
            } else if (request_kind==AI_VI_INSERT_TEXT) {
                title="PuTTY AI - Insert text in vi";
                approve_label="Insert text";
                description="Paste the text shown below into the vi buffer. Check that the editor is ready.";
            } else if (request_kind==AI_TOP_QUIT || request_kind==AI_VI_QUIT) {
                title=vi?"PuTTY AI - Quit vi":"PuTTY AI - Quit top (q)";
                approve_label=vi?"Quit vi":"Send q";
                description=vi?"Send Escape, :qa, and Enter to vi. Unsaved changes may prevent exit.":
                    "Send one q to the top task in this window to request exit.";
            } else if (request_kind==AI_SPECIAL_KEY) {
                AiInputKind kind=input_kind(&b->incoming);
                const AiSpecialKey *key=ai_task_special_key(kind);
                const char *effect="The effect depends on the application and terminal mode.";
                if (kind==AI_INPUT_KEY_CTRL_Q)
                    effect="May resume output with XON/XOFF or invoke an application binding.";
                else if (kind==AI_INPUT_KEY_CTRL_S)
                    effect="May pause output with XON/XOFF or invoke an application binding.";
                else if (kind==AI_INPUT_KEY_CTRL_D)
                    effect="May signal end of input or invoke an application binding.";
                else if (kind==AI_INPUT_KEY_CTRL_Z)
                    effect="May suspend the task. Disable AI to close its channel locally if needed.";
                title="PuTTY AI - Send key";
                snprintf(key_label,sizeof(key_label),"Send %s",key->name);
                approve_label=key_label;
                if (kind==AI_INPUT_KEY_UNDO)
                    snprintf(action_text,sizeof(action_text),"Send Escape then u to undo the previous vi change.");
                else if (kind==AI_INPUT_KEY_REDO)
                    snprintf(action_text,sizeof(action_text),"Send Escape then Ctrl+R to redo the vi change.");
                else if (kind==AI_INPUT_KEY_INSERT_MODE)
                    snprintf(action_text,sizeof(action_text),"Send Escape then i to enter vi insert mode.");
                else if (kind==AI_INPUT_KEY_DISCARD_AND_QUIT) {
                    title="PuTTY AI - Discard vi changes";
                    approve_label="Discard changes";
                    snprintf(action_text,sizeof(action_text),"Send Escape, :qa!, and Enter to discard unsaved vi changes and quit.");
                }
                else
                    snprintf(action_text,sizeof(action_text),"Send one %s to the %s task. %s",
                             key->name,program,effect);
                description=action_text;
            } else {
                title=vi?"PuTTY AI - Interrupt vi (Ctrl+C)":"PuTTY AI - Interrupt top (Ctrl+C)";
                approve_label="Send Ctrl+C";
                description=vi?"Send one Ctrl+C to the vi task in this window.":
                    "Send one Ctrl+C to the top task in this window.";
            }
            snprintf(text,sizeof(text),"ACTION: %s\nPROGRAM: %s\n\nSOURCE: %.*s\n"
                "CLIENT CONNECTION: %02x%02x%02x%02x\n"
                "TARGET: %s\nPuTTY session %02x%02x%02x%02x\n"
                "TASK: %02x%02x%02x%02x%s%.*s\n\n%s%s\n\n"
                "Confirm the SSH endpoint. The task uses the SSH login account and a new session environment.%s%s%s",
                description,program,(int)source_len,(const char *)b->payload+AI_COMMAND_PREFIX,
                b->payload[0],b->payload[1],b->payload[2],b->payload[3],b->top_target,
                b->session[0],b->session[1],b->session[2],b->session[3],
                b->incoming.request[0],b->incoming.request[1],b->incoming.request[2],b->incoming.request[3],
                b->task.path_length?"\nFILE: ":"",(int)b->task.path_length,path,
                ssh_command?"SSH command: ":"",ssh_command?ssh_command:"",
                request_kind==AI_VI_INSERT_TEXT?"\n\n--- BEGIN TEXT ---\n":"",
                request_kind==AI_VI_INSERT_TEXT?(const char *)b->payload+b->task.text_offset:"",
                request_kind==AI_VI_INSERT_TEXT?"\n--- END TEXT ---":"");
        } else {
        title=interrupt?"PuTTY AI - Interrupt task (Ctrl+C)":"PuTTY AI command approval";
        if (interrupt) approve_label="Send Ctrl+C";
        snprintf(text,sizeof(text),
            "%s"
            "SOURCE: %.*s\n"
            "CLIENT CONNECTION: %02x%02x%02x%02x\n"
            "TARGET: PuTTY session %02x%02x%02x%02x (this window)\n\n"
            "%s\n\n"
            "--- BEGIN %s ---\n%s\n--- END %s ---",
            interrupt?"ACTION: Send Ctrl+C to interrupt the current task.\n\n":"",
            (int)source_len,(const char *)b->payload+AI_COMMAND_PREFIX,
            b->payload[0],b->payload[1],b->payload[2],b->payload[3],
            b->session[0],b->session[1],b->session[2],b->session[3],
            interrupt ?
                "Verify the original command is still running in this window. Send Ctrl+C requests one keypress to the foreground terminal." :
                "Verify an idle shell or device CLI with an empty input line. Reject at a password or MFA prompt, or while another program is running. Approve sends exactly this input with Enter.",
            interrupt?"ORIGINAL COMMAND":"SCRIPT",command,
            interrupt?"ORIGINAL COMMAND":"SCRIPT");
        }
    }
    c=HeapAlloc(GetProcessHeap(),HEAP_ZERO_MEMORY,sizeof(*c));
    if (!c) { if (b) finish_request(b,not_sent(b)); return; }
    c->owner=owner; c->bridge=b; c->hwnd=hwnd; c->menu=menu; c->observation=observation;
    c->send=send; c->context=ctx; c->expected_epoch=b?b->incoming.epoch:0;
    if (b) InterlockedIncrement(&b->references);
    prompt=ai_approval_open(hwnd,title,text,approve_label,consent_complete,c);
    if (!prompt) {
        if (b) { finish_request(b,not_sent(b)); release(b); }
        HeapFree(GetProcessHeap(),0,c);
    } else if (b) b->approval=prompt;
    ai_bridge_dispatch(*owner,hwnd,menu);
}
void ai_bridge_toggle_observation(AiBridge **owner,HWND hwnd,HMENU menu,AiSend send,void *ctx)
{
    if (*owner) {
        ai_bridge_stop(owner);
        ai_bridge_dispatch(NULL,hwnd,menu);
    } else {
        open_approval(owner,hwnd,menu,1,send,ctx);
    }
}
void ai_bridge_offer_pending(AiBridge **owner,HWND hwnd,HMENU menu,AiSend send,void *ctx)
{
    AiBridge *b=*owner;
    if (!b || !load(&b->online) || b->approval) return;
    if (InterlockedCompareExchange(&b->inbox_state,INBOX_SENDING,INBOX_INTERRUPT_SEND)==INBOX_INTERRUPT_SEND) {
        DWORD ack=not_sent(b);
        if (request_ready(b,send,ctx)) {
            if (child_request(b->incoming.kind)) {
                if (send(ctx,input_kind(&b->incoming),AI_PROGRAM_NONE,
                         (const char *)b->incoming.request,16))
                    ack=ACK_CHILD_ACCEPTED;
            } else if (b->incoming.kind==AI_TERMINAL_ATTACH) {
                b->terminal_offset=b->ring.sequence;
                bind_terminal(b,b->incoming.request,b->incoming.epoch);
                ack=ACK_TERMINAL_ATTACHED;
            } else if (b->incoming.kind==AI_TERMINAL_INPUT) {
                size_t offset=terminal_payload_offset(b)+AI_TERMINAL_INPUT_PREFIX;
                size_t length=b->incoming.length-offset;
                if (send(ctx,input_kind(&b->incoming),AI_PROGRAM_NONE,(const char *)b->payload+offset,length))
                    ack=ACK_TERMINAL_INPUT_SENT;
                SecureZeroMemory(b->payload+offset,length);
            } else if (b->incoming.kind==AI_VI_INSERT_TEXT) {
                size_t offset=b->task.text_offset;
                size_t length=b->incoming.length-offset;
                char input[16+AI_VI_TEXT_MAX];
                memcpy(input,b->incoming.request,16);
                memcpy(input+16,b->payload+offset,length);
                if (send(ctx,AI_INPUT_VI_TEXT,b->task.program,input,16+length))
                    ack=ACK_TOP_KEY_SENT;
                SecureZeroMemory(input,sizeof(input));
            } else if (b->incoming.kind==AI_VI_SAVE) {
                if (send_path_input(send,ctx,b,AI_INPUT_VI_SAVE))
                    ack=ACK_TOP_KEY_SENT;
            } else if (task_key(b->incoming.kind)) {
                if (send(ctx,input_kind(&b->incoming),b->task.program,
                         (const char *)b->incoming.request,16))
                    ack=ACK_TOP_KEY_SENT;
            } else if (b->last_sent_epoch==b->incoming.epoch &&
                       !memcmp(b->last_sent,b->incoming.request,16)) {
                b->interrupt_offset=b->ring.sequence;
                if (send(ctx,AI_INPUT_INTERRUPT,AI_PROGRAM_NONE,"\003",1)) ack=ACK_INTERRUPT_SENT;
            }
        }
        finish_request(b,ack);
    } else if (load(&b->inbox_state)==INBOX_WAITING) {
        if (!b->incoming_auto_run && b->incoming.kind!=AI_TERMINAL_DETACH) {
            open_approval(owner,hwnd,menu,0,send,ctx);
        } else if (InterlockedCompareExchange(&b->inbox_state,INBOX_SENDING,INBOX_WAITING)==INBOX_WAITING) {
            DWORD ack=request_ready(b,send,ctx)?authorize_request(b,send,ctx):not_sent(b);
            finish_request(b,ack);
        }
    }
}
