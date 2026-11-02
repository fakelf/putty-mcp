#include "approval.h"
typedef struct Approval {
    HWND owner;
    void (*complete)(void *,int);
    void *context;
    int finished;
    int initialized;
    const char *text;
} Approval;
static const wchar_t property[]=L"PuTTY.AI.Approval";
void ai_bridge_cancel_approval(HWND owner)
{
    HWND prompt=(HWND)GetPropW(owner,property);
    if (prompt) DestroyWindow(prompt);
}
static LRESULT CALLBACK approval_window(HWND hwnd,UINT msg,WPARAM wp,LPARAM lp)
{
    Approval *a=(Approval *)GetWindowLongPtr(hwnd,GWLP_USERDATA);
    if (msg==WM_CREATE) {
        HWND button,edit; wchar_t wide[8192],display[16384]; int count,i,j=0;
        Approval *initial=((CREATESTRUCT *)lp)->lpCreateParams;
        a=HeapAlloc(GetProcessHeap(),HEAP_ZERO_MEMORY,sizeof(*a));
        if (!a) return -1;
        *a=*initial;
        SetWindowLongPtr(hwnd,GWLP_USERDATA,(LONG_PTR)a);
        count=MultiByteToWideChar(CP_UTF8,MB_ERR_INVALID_CHARS,a->text,-1,wide,8192);
        if (!count) return -1;
        for(i=0;i<count;i++) { if(wide[i]=='\n' && (!i || wide[i-1]!='\r')) display[j++]='\r'; display[j++]=wide[i]; }
        edit=CreateWindowExW(WS_EX_CLIENTEDGE,L"EDIT",display,WS_CHILD|WS_VISIBLE|WS_TABSTOP|WS_VSCROLL|
            ES_MULTILINE|ES_READONLY|ES_AUTOVSCROLL,18,16,644,360,hwnd,(HMENU)3,GetModuleHandle(NULL),NULL);
        if (!edit) return -1;
        SendMessage(edit,WM_SETFONT,(WPARAM)GetStockObject(DEFAULT_GUI_FONT),TRUE);
        CreateWindowA("BUTTON","Approve",WS_CHILD|WS_VISIBLE|WS_TABSTOP,440,396,100,32,hwnd,(HMENU)1,GetModuleHandle(NULL),NULL);
        button=CreateWindowA("BUTTON","Reject",WS_CHILD|WS_VISIBLE|WS_TABSTOP,558,396,100,32,hwnd,(HMENU)2,GetModuleHandle(NULL),NULL);
        SetFocus(button); return 0;
    }
    if (a && ((msg==WM_COMMAND && (LOWORD(wp)==1 || LOWORD(wp)==2)) || msg==WM_CLOSE)) {
        if (!a->finished) {
            a->finished=1;
            RemovePropW(a->owner,property);
            a->complete(a->context,msg==WM_COMMAND && LOWORD(wp)==1);
        }
        DestroyWindow(hwnd); return 0;
    }
    if (a && msg==WM_NCDESTROY) {
        if (a->initialized && !a->finished) { a->finished=1; RemovePropW(a->owner,property); a->complete(a->context,0); }
        SetWindowLongPtr(hwnd,GWLP_USERDATA,0);
        HeapFree(GetProcessHeap(),0,a); return 0;
    }
    return DefWindowProc(hwnd,msg,wp,lp);
}
HWND ai_approval_open(HWND owner,const char *title,const char *text,
                      void (*complete)(void *,int),void *context)
{
    WNDCLASSA wc; Approval initial, *a; HWND hwnd; RECT rect;
    HWND existing=(HWND)GetPropW(owner,property);
    if (existing) { SetForegroundWindow(existing); return NULL; }
    ZeroMemory(&wc,sizeof(wc)); wc.lpfnWndProc=approval_window;
    wc.lpszClassName="PuTTYAIApprovalV1"; wc.hInstance=GetModuleHandle(NULL);
    wc.hCursor=LoadCursor(NULL,IDC_ARROW); wc.hbrBackground=(HBRUSH)(COLOR_BTNFACE+1);
    if (!RegisterClassA(&wc) && GetLastError()!=ERROR_CLASS_ALREADY_EXISTS) return NULL;
    ZeroMemory(&initial,sizeof(initial));
    initial.owner=owner; initial.complete=complete; initial.context=context; initial.text=text;
    GetWindowRect(owner,&rect);
    hwnd=CreateWindowExA(WS_EX_DLGMODALFRAME|WS_EX_TOOLWINDOW,wc.lpszClassName,title,
        WS_POPUP|WS_CAPTION|WS_SYSMENU,rect.left+40,rect.top+40,700,485,owner,NULL,wc.hInstance,&initial);
    if (!hwnd) return NULL;
    a=(Approval *)GetWindowLongPtr(hwnd,GWLP_USERDATA);
    if (!SetPropW(owner,property,hwnd)) { DestroyWindow(hwnd); return NULL; }
    a->initialized=1;
#ifndef PUTTY_AI_TEST_HEADLESS
    ShowWindow(hwnd,SW_SHOWNORMAL); SetForegroundWindow(hwnd);
#endif
    return hwnd;
}
