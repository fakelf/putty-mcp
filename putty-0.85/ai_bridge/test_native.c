/* Synthetic frontend for automated IPC/fault tests. No network or SSH backend.
 * Local approvals are simulated ONLY in this separate test executable.
 * It is deliberately excluded from the deployable runtime package.
 */
#define PUTTY_AI_BRIDGE
#include "bridge.c"
static AiBridge *test_bridge;
static HWND test_hwnd;
static HMENU test_menu;
static unsigned long ticks;
static int auto_approve=1;
static int test_send(void *ctx,const char *line,size_t length)
{
    AiBridge *b=test_bridge;
    unsigned i; char token[33],output[256]; (void)ctx;
    if (!line) return 1;
    if (!b || !length || !strstr(line,"printf '\\036PAI:")) return 0;
    for(i=0;i<16;i++) snprintf(token+2*i,3,"%02x",b->incoming.request[i]);
    snprintf(output,sizeof(output),"\036PAI:%s:BEGIN\037\nSIMULATED %s result\n\036PAI:%s:END:0\037\n",token,b->incoming.kind==14?"custom":"uname",token);
    ai_bridge_capture(b,output,strlen(output));
    return 1;
}
#define TEST_INPUT (WM_APP+80)
static void report(void)
{
    unsigned i;
    printf("{\"event\":\"state\",\"ticks\":%lu,\"session_hex\":\"",ticks);
    if (test_bridge) for(i=0;i<16;i++) printf("%02x",test_bridge->session[i]);
    printf("\",\"online\":%ld,\"dropped\":%lld,\"approval_open\":%s}\n",test_bridge?load(&test_bridge->online):0,
        test_bridge?InterlockedCompareExchange64(&test_bridge->ring.dropped,0,0):0,
        test_bridge && test_bridge->approval?"true":"false");
    fflush(stdout);
}
static DWORD WINAPI test_input(void *unused)
{
    char text[64]; (void)unused;
    while(fgets(text,sizeof(text),stdin)) {
        if (!strcmp(text,"quit\n")) { PostMessage(test_hwnd,WM_CLOSE,0,0); return 0; }
        if (!strcmp(text,"ping\n")) PostMessage(test_hwnd,TEST_INPUT,'p',0);
        if (!strcmp(text,"human\n")) PostMessage(test_hwnd,TEST_INPUT,'h',0);
        if (!strcmp(text,"off\n")) PostMessage(test_hwnd,TEST_INPUT,'f',0);
        if (!strcmp(text,"on\n")) PostMessage(test_hwnd,TEST_INPUT,'o',0);
        if (!strcmp(text,"flood\n")) PostMessage(test_hwnd,TEST_INPUT,'x',0);
        if (!strcmp(text,"hold\n")) PostMessage(test_hwnd,TEST_INPUT,'d',0);
        if (!strcmp(text,"allow\n")) PostMessage(test_hwnd,TEST_INPUT,'a',0);
        if (!strcmp(text,"reject\n")) PostMessage(test_hwnd,TEST_INPUT,'r',0);
    }
    PostMessage(test_hwnd,WM_CLOSE,0,0); return 0;
}
static LRESULT CALLBACK test_window(HWND hwnd,UINT msg,WPARAM wp,LPARAM lp)
{
    if (msg==WM_TIMER && wp==9) {
        ticks++;
        ai_bridge_capture(test_bridge,"synthetic-native-output\n",24);
        return 0;
    }
    if (msg==WM_AI_BRIDGE || (msg==WM_TIMER && wp==AI_TIMER)) {
        AiBridge *b=test_bridge;
        if (!b) return 0;
        ai_bridge_dispatch(b,hwnd,test_menu);
        /* Same automatic modeless offer as production. Test-only auto-click. */
        ai_bridge_offer_pending(&test_bridge,hwnd,test_menu,1,test_send,NULL);
        if (auto_approve && b->approval && load(&b->inbox_state)==4) PostMessage(b->approval,WM_COMMAND,1,0);
        return 0;
    }
    if (msg==TEST_INPUT) {
        if (wp=='r' && test_bridge && test_bridge->approval) PostMessage(test_bridge->approval,WM_COMMAND,2,0);
        if (wp=='d') auto_approve=0;
        if (wp=='a') { auto_approve=1; if (test_bridge && test_bridge->approval) PostMessage(test_bridge->approval,WM_COMMAND,1,0); }
        if (wp=='h') ai_bridge_human_input(test_bridge);
        if (wp=='f') ai_bridge_stop(&test_bridge);
        if (wp=='o' && !test_bridge) test_bridge=start(hwnd);
        if (wp=='x' && test_bridge) {
            char data[16384]; unsigned i; memset(data,'Z',sizeof(data));
            for(i=0;i<1024;i++) ai_bridge_capture(test_bridge,data,sizeof(data));
        }
        report(); return 0;
    }
    if (msg==WM_CLOSE) { ai_bridge_stop(&test_bridge); DestroyWindow(hwnd); return 0; }
    if (msg==WM_DESTROY) { PostQuitMessage(0); return 0; }
    return DefWindowProc(hwnd,msg,wp,lp);
}
int main(void)
{
    WNDCLASSA wc; MSG msg; HANDLE input;
    memset(&wc,0,sizeof(wc)); wc.lpfnWndProc=test_window; wc.lpszClassName="PuTTY AI synthetic test";
    wc.hInstance=GetModuleHandle(NULL); if (!RegisterClassA(&wc)) return 1;
    test_hwnd=CreateWindowA(wc.lpszClassName,"Synthetic terminal (no remote connection)",0,0,0,0,0,HWND_MESSAGE,NULL,wc.hInstance,NULL);
    if (!test_hwnd) return 1;
    test_menu=CreatePopupMenu(); ai_bridge_add_menu(test_menu);
    test_bridge=start(test_hwnd); if (!test_bridge) return 1;
    SetTimer(test_hwnd,9,20,NULL); report();
    input=CreateThread(NULL,0,test_input,NULL,0,NULL); if (!input) return 1; CloseHandle(input);
    while(GetMessage(&msg,NULL,0,0)>0) { TranslateMessage(&msg); DispatchMessage(&msg); }
    return 0;
}
