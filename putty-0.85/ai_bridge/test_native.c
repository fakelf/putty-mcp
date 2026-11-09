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
static int stream_test_noise=1;
static int top_running;
static int top_taken;
static AiTaskProgram top_program;
static const AiSpecialKey *last_special_key;
static unsigned special_key_count;
static unsigned text_insert_count;
static unsigned file_save_count;
static unsigned approval_count;
static unsigned terminal_input_count, terminal_revision;
static unsigned terminal_page;
static int page_next_config;
static char foreground_text[AI_TERMINAL_TEXT_MAX+1];
static int vi_text_fixture_seen;
static unsigned char top_id[16];
static const char vi_text_fixture[]="draft :qa!\n\t\xe9\x9b\xaa";
static const char vi_text_delivered[]="vi text fixture delivered\r\n";
static const char vi_file_fixture[]="/home/fixture/test '\xc3\xa9'.sh";
static const char vi_file_save_dispatched[]="vi file save dispatched\r\n";
typedef struct TestChild {
    HWND hwnd;
    AiBridge *bridge;
    int close_committed;
    unsigned terminal_input_count;
    unsigned char close_id[16];
} TestChild;
static TestChild test_children[4];
static int fail_next_child;
static int is_file_fixture(const char *input,size_t length)
{
    return length==16+sizeof(vi_file_fixture)-1 &&
        !memcmp(input+16,vi_file_fixture,sizeof(vi_file_fixture)-1);
}
static const char default_prompt[]="<branch-switch-lab>";
static const char config_prompt[]="[branch-switch-lab]";
static const char *root_prompt=default_prompt;

static void complete_synthetic(AiBridge *b,const char *prompt,int interrupted)
{
    char output[80];
    snprintf(output,sizeof(output),"%s\r\n\033[?2004h%s \033[K",interrupted?"^C":"",prompt);
    ai_bridge_capture(b,output,strlen(output));
}
static int fixture_terminal_action(AiBridge *b,AiInputKind kind,const char *input,
                                   size_t length,TestChild *child)
{
    if (!b || (!child && top_running)) return 0;
    if (kind==AI_INPUT_ATTACH) return !length;
    if (kind==AI_INPUT_SCREEN_REFRESH) {
        if (!ai_bridge_terminal_active(b)) return 0;
        char screen[AI_TERMINAL_TEXT_MAX+150];
        if (child) {
            snprintf(screen,sizeof(screen),"Synthetic child terminal\nChild actions: %u",
                     child->terminal_input_count);
        } else if (terminal_page) {
            snprintf(screen,sizeof(screen),"H3C fixture page %u\n----more----\nRevision %u",
                     terminal_page,terminal_revision);
        } else {
            snprintf(screen,sizeof(screen),"Synthetic foreground\n%s\n%s",foreground_text,root_prompt);
        }
        ai_bridge_terminal_screen(b,screen,strlen(screen),80,24,1,2,2);
        return 1;
    }
    if (!length) return 1;
    if (child) {
        if (kind!=AI_INPUT_KEY || length!=5 || memcmp(input,"space",5)) return 0;
        ++child->terminal_input_count;
        return 1;
    }
    if (kind==AI_INPUT_KEY) {
        if (!terminal_page) return 0;
        if (length==5 && !memcmp(input,"space",5) && terminal_page==1) {
            terminal_page=2;
            static const char page[]="\r\npage 2 content\r\n----more----";
            ai_bridge_capture(b,page,sizeof(page)-1);
        } else if (length==5 && !memcmp(input,"enter",5) && terminal_page==2) {
            terminal_page=3;
            static const char page[]="\r\nnext line\r\n----more----";
            ai_bridge_capture(b,page,sizeof(page)-1);
        } else if (length==6 && !memcmp(input,"ctrl+c",6)) {
            terminal_page=0;
            complete_synthetic(b,root_prompt,1);
        } else return 0;
    } else {
        if (length>sizeof(foreground_text)-1) return 0;
        memcpy(foreground_text,input,length);
        foreground_text[length]=0;
    }
    ++terminal_input_count;
    return 1;
}
static const char *synthetic_echo(AiBridge *b,const char *line,size_t length,const char *prompt)
{
    size_t offset=AI_COMMAND_PREFIX+b->payload[16];
    size_t expected_length=b->incoming.length-offset;
    const char *command=(const char *)b->payload+offset;
    if (!expected_length || !length) return NULL;
    size_t terminator=command[expected_length-1]!='\n';
    if (length!=expected_length+terminator || line[length-1]!='\r') return NULL;
    for (size_t i=0;i<expected_length;i++)
        if (line[i]!=(command[i]=='\n'?'\r':command[i])) return NULL;

    ai_bridge_command_prompt(b,prompt,strlen(prompt));
    char echo[AI_INPUT_LINE_MAX*2];
    size_t used=0;
    for (size_t i=0;i<length;i++) {
        echo[used++]=line[i];
        if (line[i]=='\r') echo[used++]='\n';
    }
    ai_bridge_capture(b,echo,used);
    return command;
}
static int test_child_send(void *ctx,AiInputKind kind,AiTaskProgram program,
                           const char *input,size_t length)
{
    TestChild *child=ctx;
    AiBridge *b=child->bridge;
    if (program || !b || child->close_committed) return 0;
    if (kind==AI_INPUT_CHILD_CLOSE) {
        if (length==16) {
            child->close_committed=1;
            memcpy(child->close_id,input,16);
        }
        return length==0 || length==16;
    }
    if (kind==AI_INPUT_INTERRUPT) {
        if (!input) return length==0;
        if (length!=1 || *input!='\003') return 0;
        complete_synthetic(b,default_prompt,1);
        return 1;
    }
    if (kind>=AI_INPUT_ATTACH)
        return fixture_terminal_action(b,kind,input,length,child);
    if (kind!=AI_INPUT_SCRIPT) return 0;
    if (!length) return 1;
    const char *command=synthetic_echo(b,input,length,default_prompt);
    if (!command) return 0;
    static const char output[]="SIMULATED child command result\r\n";
    ai_bridge_capture(b,output,sizeof(output)-1);
    if (!strstr(command,"synthetic-long-task")) complete_synthetic(b,default_prompt,0);
    return 1;
}
static LRESULT CALLBACK test_child_window(HWND hwnd,UINT msg,WPARAM wp,LPARAM lp)
{
    TestChild *child=(TestChild *)GetWindowLongPtr(hwnd,GWLP_USERDATA);
    if (msg==WM_NCCREATE) {
        child=((CREATESTRUCT *)lp)->lpCreateParams;
        SetWindowLongPtr(hwnd,GWLP_USERDATA,(LONG_PTR)child);
    }
    if (child && (msg==WM_CLOSE || (msg==WM_AI_CLOSE_CHILD && child->close_committed))) {
        int confirmed=msg==WM_AI_CLOSE_CHILD && child->bridge!=NULL;
        ai_bridge_stop(&child->bridge);
        DestroyWindow(hwnd);
        child->hwnd=NULL;
        child->close_committed=0;
        if (confirmed) ai_bridge_child_closed(test_bridge,child->close_id);
        return 0;
    }
    if (child && child->bridge && (msg==WM_AI_BRIDGE || (msg==WM_TIMER && wp==AI_TIMER))) {
        ai_bridge_dispatch(child->bridge,hwnd,NULL);
        int approval_open=child->bridge->approval!=NULL;
        ai_bridge_offer_pending(&child->bridge,hwnd,NULL,test_child_send,child);
        fixture_terminal_action(child->bridge,AI_INPUT_SCREEN_REFRESH,NULL,0,child);
        if (!approval_open && child->bridge->approval) ++approval_count;
        if (auto_approve && child->bridge->approval && load(&child->bridge->inbox_state)==INBOX_OPEN)
            PostMessage(child->bridge->approval,WM_COMMAND,1,0);
        return 0;
    }
    return DefWindowProc(hwnd,msg,wp,lp);
}
static void test_child_revoke(void)
{
    for (unsigned i=0;i<4;i++) ai_bridge_stop(&test_children[i].bridge);
}
static int test_child_open(AiBridge *parent,const char *id,size_t length)
{
    if (!parent || (length && length!=16)) return 0;
    for (unsigned i=0;i<4;i++) {
        TestChild *child=&test_children[i];
        if (child->hwnd) continue;
        if (!length) return 1;
        if (fail_next_child) {
            fail_next_child=0;
            ai_bridge_child_failed(parent,(const unsigned char *)id,2);
            return 1;
        }
        child->hwnd=CreateWindowA("PuTTY AI synthetic child","Synthetic child",0,
                                  0,0,0,0,HWND_MESSAGE,NULL,GetModuleHandle(NULL),child);
        if (!child->hwnd) return 0;
        child->bridge=ai_bridge_start_child(child->hwnd,parent,(const unsigned char *)id);
        if (!child->bridge) {
            DestroyWindow(child->hwnd);
            child->hwnd=NULL;
            return 0;
        }
        static const char output[]="synthetic-child-output\n";
        ai_bridge_capture(child->bridge,output,sizeof(output)-1);
        return 1;
    }
    return 0;
}
static int test_send(void *ctx,AiInputKind kind,AiTaskProgram program,const char *line,size_t length)
{
    AiBridge *b=test_bridge;
    char output[256]; (void)ctx;
    if (kind==AI_INPUT_CHILD_OPEN) return test_child_open(b,line,length);
    if (kind>=AI_INPUT_ATTACH)
        return fixture_terminal_action(b,kind,line,length,NULL);
    if (program) {
        if (!b || !line) return 0;
        if (kind==AI_INPUT_TASK_START) {
            if (top_running) return 0;
            if (!length) return 1;
            if (program==AI_PROGRAM_VI_FILE ? !is_file_fixture(line,length) : length!=16) return 0;
            memcpy(top_id,line,16);
            top_running=1;
            top_taken=0;
            top_program=program;
            last_special_key=NULL;
            special_key_count=0;
            text_insert_count=0;
            file_save_count=0;
            vi_text_fixture_seen=0;
            ai_bridge_top_state(b,top_id,AI_TOP_RUNNING,0,0);
            return 1;
        }
        if (!top_running || top_program!=program || top_taken || memcmp(top_id,line,16)) return 0;
        if (kind==AI_INPUT_VI_SAVE_CHECK || kind==AI_INPUT_VI_SAVE) {
            if (program!=AI_PROGRAM_VI_FILE || !is_file_fixture(line,length)) return 0;
            if (kind==AI_INPUT_VI_SAVE) {
                ++file_save_count;
                ai_bridge_top_screen(b,top_id,vi_file_save_dispatched,
                                     sizeof(vi_file_save_dispatched)-1,0,0,1,1);
            }
            return 1;
        }
        if (!length) return 1;
        if (kind==AI_INPUT_VI_TEXT) {
            if (!ai_task_is_vi(program) || length<=16 ||
                !ai_text_validate(line+16,length-16)) return 0;
            ++text_insert_count;
            if (length==16+sizeof(vi_text_fixture)-1 &&
                !memcmp(line+16,vi_text_fixture,sizeof(vi_text_fixture)-1)) {
                vi_text_fixture_seen=1;
                ai_bridge_top_screen(b,top_id,vi_text_delivered,sizeof(vi_text_delivered)-1,0,0,1,1);
            }
            return 1;
        }
        if (length!=16) return 0;
        const AiSpecialKey *key=ai_task_special_key(kind);
        if (key) {
            const char *program_name=ai_task_is_vi(program)?"vi":"top";
            last_special_key=key;
            ++special_key_count;
            snprintf(output,sizeof(output),"%s key: %s\r\n",program_name,key->name);
            ai_bridge_top_screen(b,top_id,output,strlen(output),0,0,1,ai_task_is_vi(program));
            if (kind==AI_INPUT_KEY_DISCARD_AND_QUIT) {
                top_running=0;
                ai_bridge_top_state(b,top_id,AI_TOP_EXITED,0,0);
            }
            return 1;
        }
        top_running=0;
        ai_bridge_top_state(b,top_id,kind==AI_INPUT_TASK_QUIT?AI_TOP_EXITED:AI_TOP_SIGNALLED,0,0);
        return 1;
    }
    if (!line) return 1;
    if (kind==AI_INPUT_INTERRUPT) {
        if (!b || length!=1 || *line!='\003') return 0;
        complete_synthetic(b,root_prompt,1);
        return 1;
    }
    if (!b || !length) return 0;
    stream_test_noise=0;
    const char *command=synthetic_echo(b,line,length,root_prompt);
    if (!command) return 0;
    static const char shell_output[]="SIMULATED command result\r\n";
    static const char config_output[]="#\r\nsysname branch-switch-lab\r\n#\r\n";
    if (!strcmp(command,"display current-configuration"))
        ai_bridge_capture(b,config_output,sizeof(config_output)-1);
    else
        ai_bridge_capture(b,shell_output,sizeof(shell_output)-1);
    if (!strcmp(command,"system-view")) root_prompt=config_prompt;
    if (!strcmp(command,"return")) root_prompt=default_prompt;
    if (page_next_config && !strcmp(command,"display current-configuration")) {
        static const char pager[]="----more----";
        page_next_config=0;
        terminal_page=1;
        ai_bridge_capture(b,pager,sizeof(pager)-1);
    } else if (!strstr(command,"synthetic-long-task")) complete_synthetic(b,root_prompt,0);
    return 1;
}
#define TEST_INPUT (WM_APP+80)
static void report(void)
{
    unsigned i;
    wchar_t approval_text[8192]=L"";
    wchar_t title[128]=L"", approve_label[32]=L"";
    const char *top_prompt="none";
    unsigned child_count=0,child_prompt_count=0,child_close_prompts=0;
    for (i=0;i<4;i++) {
        TestChild *child=&test_children[i];
        if (child->hwnd) ++child_count;
        if (child->bridge && child->bridge->approval) {
            wchar_t child_title[128];
            ++child_prompt_count;
            GetWindowTextW(child->bridge->approval,child_title,128);
            if (!wcscmp(child_title,L"PuTTY AI - Close child terminal")) ++child_close_prompts;
        }
    }
    if (test_bridge && test_bridge->approval) {
        HWND approval=test_bridge->approval;
        GetWindowTextW(approval,title,128);
        GetWindowTextW(GetDlgItem(approval,3),approval_text,8192);
        GetWindowTextW(GetDlgItem(approval,1),approve_label,32);
    }
    if (wcsstr(approval_text,L"TARGET: test @ synthetic (port 22)")) {
        int file_visible=wcsstr(approval_text,L"FILE: /home/fixture/test '\x00e9'.sh\r\n") &&
            wcsstr(approval_text,L"SOURCE: Harness test (local configuration)\r\n");
        if (!wcscmp(title,L"PuTTY AI - Open child terminal") && !wcscmp(approve_label,L"Open window"))
            top_prompt="child_open";
        else if (!wcscmp(title,L"PuTTY AI - Start top") && !wcscmp(approve_label,L"Start top"))
            top_prompt="start";
        else if (!wcscmp(title,L"PuTTY AI - Quit top (q)") && !wcscmp(approve_label,L"Send q"))
            top_prompt="quit";
        else if (!wcscmp(title,L"PuTTY AI - Interrupt top (Ctrl+C)") && !wcscmp(approve_label,L"Send Ctrl+C"))
            top_prompt="interrupt";
        else if (!wcscmp(title,L"PuTTY AI - Start vi") && !wcscmp(approve_label,L"Start vi"))
            top_prompt="vi_start";
        else if (!wcscmp(title,L"PuTTY AI - Open remote file") &&
                 !wcscmp(approve_label,L"Open file") && file_visible)
            top_prompt="vi_file_open";
        else if (!wcscmp(title,L"PuTTY AI - Save remote file") &&
                 !wcscmp(approve_label,L"Save file") && file_visible)
            top_prompt="vi_file_save";
        else if (!wcscmp(title,L"PuTTY AI - Quit vi") && !wcscmp(approve_label,L"Quit vi"))
            top_prompt="vi_quit";
        else if (!wcscmp(title,L"PuTTY AI - Interrupt vi (Ctrl+C)") && !wcscmp(approve_label,L"Send Ctrl+C"))
            top_prompt="vi_interrupt";
        else if (!wcscmp(title,L"PuTTY AI - Discard vi changes") &&
                 !wcscmp(approve_label,L"Discard changes") &&
                 wcsstr(approval_text,L"discard unsaved vi changes and quit"))
            top_prompt="vi_discard";
        else if (!wcscmp(title,L"PuTTY AI - Insert text in vi") &&
                 !wcscmp(approve_label,L"Insert text") &&
                 wcsstr(approval_text,L"SOURCE: Harness test (local configuration)\r\n") &&
                 wcsstr(approval_text,L"--- BEGIN TEXT ---\r\ndraft :qa!\r\n\t\x96ea\r\n--- END TEXT ---"))
            top_prompt="vi_insert_text";
        else if (!wcscmp(title,L"PuTTY AI - Send key") && !wcsncmp(approve_label,L"Send ",5))
            top_prompt="special_key";
    }
    printf("{\"event\":\"state\",\"ticks\":%lu,\"session_hex\":\"",ticks);
    if (test_bridge) for(i=0;i<16;i++) printf("%02x",test_bridge->session[i]);
    printf("\",\"online\":%ld,\"dropped\":%lld,\"approval_open\":%s,\"configured_source_visible\":%s,\"interrupt_prompt\":%s,\"top_prompt\":\"%s\",\"special_key_count\":%u,\"text_insert_count\":%u,\"file_save_count\":%u,\"child_count\":%u,\"child_prompt_count\":%u,\"child_close_prompts\":%u,\"approval_count\":%u,\"terminal_input_count\":%u}\n",test_bridge?load(&test_bridge->online):0,
        test_bridge?InterlockedCompareExchange64(&test_bridge->ring.dropped,0,0):0,
        test_bridge && test_bridge->approval?"true":"false",
        wcsstr(approval_text,L"SOURCE: Harness test (local configuration)") &&
        wcsstr(approval_text,L"TARGET: PuTTY session")?"true":"false",
        !wcscmp(title,L"PuTTY AI - Interrupt task (Ctrl+C)") &&
        !wcscmp(approve_label,L"Send Ctrl+C") &&
        wcsstr(approval_text,L"ACTION: Send Ctrl+C to interrupt the current task.")==approval_text &&
        wcsstr(approval_text,L"--- BEGIN ORIGINAL COMMAND ---")?"true":"false",
        top_prompt,special_key_count,text_insert_count,file_save_count,
        child_count,child_prompt_count,child_close_prompts,approval_count,terminal_input_count);
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
        if (!strcmp(text,"finish\n")) PostMessage(test_hwnd,TEST_INPUT,'c',0);
        if (!strcmp(text,"top-finish\n")) PostMessage(test_hwnd,TEST_INPUT,'t',0);
        if (!strcmp(text,"top-exit-report\n")) PostMessage(test_hwnd,TEST_INPUT,'e',0);
        if (!strcmp(text,"child-mode-on\n")) PostMessage(test_hwnd,TEST_INPUT,'m',0);
        if (!strcmp(text,"child-mode-off\n")) PostMessage(test_hwnd,TEST_INPUT,'n',0);
        if (!strcmp(text,"fail-next-child\n")) PostMessage(test_hwnd,TEST_INPUT,'j',0);
        if (!strcmp(text,"finish-child\n")) PostMessage(test_hwnd,TEST_INPUT,'z',0);
        if (!strcmp(text,"pager-on\n")) PostMessage(test_hwnd,TEST_INPUT,'g',0);
        if (!strcmp(text,"repaint\n")) PostMessage(test_hwnd,TEST_INPUT,'v',0);
    }
    PostMessage(test_hwnd,WM_CLOSE,0,0); return 0;
}
static LRESULT CALLBACK test_window(HWND hwnd,UINT msg,WPARAM wp,LPARAM lp)
{
    if (msg==WM_TIMER && wp==9) {
        ticks++;
        if (stream_test_noise)
            ai_bridge_capture(test_bridge,"synthetic-native-output\n",24);
        if (top_running && !top_taken && ticks%20==0) {
            char screen[160];
            if (ai_task_is_vi(top_program))
                snprintf(screen,sizeof(screen),"vi - sample %lu\r\nvi key: %s\r\n%s%s",
                         ticks,last_special_key?last_special_key->name:"none",
                         vi_text_fixture_seen?vi_text_delivered:"",
                         file_save_count?vi_file_save_dispatched:"");
            else
                snprintf(screen,sizeof(screen),"top - sample %lu\r\nPID USER %%CPU COMMAND\r\n1 test 2.5 synthetic\r\n",ticks);
            ai_bridge_top_screen(test_bridge,top_id,screen,strlen(screen),0,0,1,ai_task_is_vi(top_program));
        }
        return 0;
    }
    if (msg==WM_AI_BRIDGE || (msg==WM_TIMER && wp==AI_TIMER)) {
        AiBridge *b=test_bridge;
        if (!b) return 0;
        ai_bridge_dispatch(b,hwnd,test_menu);
        /* Same automatic modeless offer as production. Test-only auto-click. */
        int approval_open=b->approval!=NULL;
        ai_bridge_offer_pending(&test_bridge,hwnd,test_menu,test_send,NULL);
        fixture_terminal_action(test_bridge,AI_INPUT_SCREEN_REFRESH,NULL,0,NULL);
        if (!approval_open && b->approval) ++approval_count;
        if (auto_approve && b->approval && load(&b->inbox_state)==INBOX_OPEN) PostMessage(b->approval,WM_COMMAND,1,0);
        return 0;
    }
    if (msg==TEST_INPUT) {
        if (wp=='r' && test_bridge && test_bridge->approval) PostMessage(test_bridge->approval,WM_COMMAND,2,0);
        if (wp=='d') auto_approve=0;
        if (wp=='a') { auto_approve=1; if (test_bridge && test_bridge->approval) PostMessage(test_bridge->approval,WM_COMMAND,1,0); }
        if (wp=='h') {
            ai_bridge_human_input(test_bridge);
            if (top_running) {
                top_taken=1;
                ai_bridge_top_state(test_bridge,top_id,AI_TOP_RUNNING,0,1);
            }
        }
        if (wp=='c' && test_bridge) complete_synthetic(test_bridge,root_prompt,0);
        if (wp=='t' && test_bridge && top_running) {
            top_running=0;
            ai_bridge_top_state(test_bridge,top_id,AI_TOP_EXITED,0,0);
        }
        if (wp=='e' && test_bridge && top_running)
            ai_bridge_top_state(test_bridge,top_id,AI_TOP_EXIT_STATUS_RECEIVED,0,0);
        if (wp=='a' || wp=='r') {
            for (unsigned i=0;i<4;i++) {
                AiBridge *b=test_children[i].bridge;
                if (b && b->approval) PostMessage(b->approval,WM_COMMAND,wp=='a'?1:2,0);
            }
        }
        if (wp=='m') ai_bridge_child_mode(test_bridge,1);
        if (wp=='n') { ai_bridge_child_mode(test_bridge,0); test_child_revoke(); }
        if (wp=='j') fail_next_child=1;
        if (wp=='g') page_next_config=1;
        if (wp=='v') {
            ++terminal_revision;
            fixture_terminal_action(test_bridge,AI_INPUT_SCREEN_REFRESH,NULL,0,NULL);
        }
        if (wp=='z') {
            for (unsigned i=0;i<4;i++)
                if (test_children[i].bridge) complete_synthetic(test_children[i].bridge,default_prompt,0);
        }
        if (wp=='f') { test_child_revoke(); ai_bridge_stop(&test_bridge); }
        if (wp=='o' && !test_bridge) {
            test_bridge=start(hwnd);
            stream_test_noise=1;
            root_prompt=default_prompt;
            ai_bridge_top_target(test_bridge,"test @ synthetic (port 22)");
        }
        if (wp=='x' && test_bridge) {
            char data[16384]; unsigned i; memset(data,'Z',sizeof(data));
            for(i=0;i<1024;i++) ai_bridge_capture(test_bridge,data,sizeof(data));
        }
        report(); return 0;
    }
    if (msg==WM_CLOSE) {
        for (unsigned i=0;i<4;i++)
            if (test_children[i].hwnd) SendMessage(test_children[i].hwnd,WM_CLOSE,0,0);
        ai_bridge_stop(&test_bridge); DestroyWindow(hwnd); return 0;
    }
    if (msg==WM_DESTROY) { PostQuitMessage(0); return 0; }
    return DefWindowProc(hwnd,msg,wp,lp);
}
static int editing_key_map_matches(void)
{
    static const char *const expected[] = {
        "\177", "\033[3~", "\033[A", "\033[B", "\033[D", "\033[C",
        "\033[H", "\033[F", "\033[5~", "\033[6~", "\033[2~", "\t",
        "\033[Z", "\r", "\033u", "\033\022", "\033i", "\033:qa!\r",
    };
    for (unsigned i=0;i<sizeof(expected)/sizeof(*expected);++i) {
        unsigned wire_flag=18+i;
        AiHeader request={0};
        request.kind=AI_SPECIAL_KEY;
        request.flags=wire_flag;
        const AiSpecialKey *key=ai_task_special_key(input_kind(&request));
        if (!key || strcmp(key->sequence,expected[i])) {
            fprintf(stderr,"Incorrect native input sequence for wire key %u\n",wire_flag);
            return 0;
        }
    }
    return 1;
}
static int vi_file_frames_match(void)
{
    enum { OFFSET=3, PATH_OFFSET=OFFSET+10, PATH_LENGTH=sizeof(vi_file_fixture)-1 };
    unsigned char frame[PATH_OFFSET+PATH_LENGTH+2]={0};
    AiHeader request={0};
    AiTaskPayload task={0};
    memcpy(frame+OFFSET,"vi-file",8);
    frame[OFFSET+8]=PATH_LENGTH;
    memcpy(frame+PATH_OFFSET,vi_file_fixture,PATH_LENGTH);
    request.kind=AI_VI_FILE_START;
    request.length=PATH_OFFSET+PATH_LENGTH;
    if (!parse_task_payload(&task,&request,frame,OFFSET) || task.program!=AI_PROGRAM_VI_FILE ||
        task.path_offset!=PATH_OFFSET || task.path_length!=PATH_LENGTH) return 0;
    request.kind=AI_VI_SAVE;
    if (!parse_task_payload(&task,&request,frame,OFFSET)) return 0;
    request.flags=1;
    if (parse_task_payload(&task,&request,frame,OFFSET)) return 0;
    request.flags=0;
    frame[OFFSET+8]=PATH_LENGTH+1;
    if (parse_task_payload(&task,&request,frame,OFFSET)) return 0;
    frame[OFFSET+8]=PATH_LENGTH;
    ++request.length;
    frame[PATH_OFFSET+PATH_LENGTH]='x';
    if (parse_task_payload(&task,&request,frame,OFFSET)) return 0;
    request.kind=AI_VI_INSERT_TEXT;
    if (!parse_task_payload(&task,&request,frame,OFFSET) ||
        task.text_offset!=PATH_OFFSET+PATH_LENGTH) return 0;
    frame[PATH_OFFSET+PATH_LENGTH]='\033';
    if (parse_task_payload(&task,&request,frame,OFFSET)) return 0;
    --request.length;
    request.kind=AI_VI_FILE_START;
    frame[PATH_OFFSET]='.';
    if (parse_task_payload(&task,&request,frame,OFFSET)) return 0;
    return 1;
}
static int mode_probe(void *ctx,AiInputKind kind,AiTaskProgram program,
                      const char *input,size_t length)
{
    (void)ctx;
    return kind==AI_INPUT_SCRIPT && !program && !input && !length;
}
static int child_mode_fences_old_approvals(void)
{
    AiBridge bridge={0};
    bridge.flags=F_READ|F_CONNECTED;
    bridge.incoming.kind=AI_COMMAND;
    if (!check_input(mode_probe,NULL,&bridge)) return 0;
    ai_bridge_child_mode(&bridge,1);
    bridge.incoming_approval_generation=load(&bridge.approval_generation);
    bridge.incoming.sequence=approval_generation(&bridge);
    if (check_input(mode_probe,NULL,&bridge)) return 0;
    bridge.incoming_auto_run=1;
    if (!check_input(mode_probe,NULL,&bridge)) return 0;
    ai_bridge_child_mode(&bridge,0);
    bridge.incoming_approval_generation=load(&bridge.approval_generation);
    if (check_input(mode_probe,NULL,&bridge)) return 0;
    ai_bridge_child_mode(&bridge,1);
    if (check_input(mode_probe,NULL,&bridge) || epoch(&bridge)!=0) return 0;
    bridge.incoming_approval_generation=load(&bridge.approval_generation);
    if (check_input(mode_probe,NULL,&bridge)) return 0;
    bridge.incoming.sequence=approval_generation(&bridge);
    return check_input(mode_probe,NULL,&bridge);
}
int main(void)
{
    WNDCLASSA wc; MSG msg; HANDLE input;
    char menu_label[64];
    if (!editing_key_map_matches()) return 1;
    if (!child_mode_fences_old_approvals()) return 1;
    if (!vi_file_frames_match()) {
        fputs("Incorrect native vi-file payload validation\n",stderr);
        return 1;
    }
    memset(&wc,0,sizeof(wc)); wc.lpfnWndProc=test_window; wc.lpszClassName="PuTTY AI synthetic test";
    wc.hInstance=GetModuleHandle(NULL); if (!RegisterClassA(&wc)) return 1;
    test_hwnd=CreateWindowA(wc.lpszClassName,"Synthetic terminal (no remote connection)",0,0,0,0,0,HWND_MESSAGE,NULL,wc.hInstance,NULL);
    if (!test_hwnd) return 1;
    wc.lpfnWndProc=test_child_window; wc.lpszClassName="PuTTY AI synthetic child";
    if (!RegisterClassA(&wc)) return 1;
    test_menu=CreatePopupMenu(); ai_bridge_add_menu(test_menu);
    if (GetMenuItemCount(test_menu)!=2 ||
        !GetMenuStringA(test_menu,IDM_AI_OBSERVE,menu_label,sizeof(menu_label),MF_BYCOMMAND) ||
        strcmp(menu_label,"Enable AI...")) return 1;
    test_bridge=start(test_hwnd); if (!test_bridge) return 1;
    update_menu(test_bridge,test_menu);
    if (GetMenuItemCount(test_menu)!=2 ||
        !GetMenuStringA(test_menu,IDM_AI_OBSERVE,menu_label,sizeof(menu_label),MF_BYCOMMAND) ||
        strcmp(menu_label,"Disable AI")) return 1;
    ai_bridge_top_target(test_bridge,"test @ synthetic (port 22)");
    SetTimer(test_hwnd,9,20,NULL); report();
    input=CreateThread(NULL,0,test_input,NULL,0,NULL); if (!input) return 1; CloseHandle(input);
    while(GetMessage(&msg,NULL,0,0)>0) { TranslateMessage(&msg); DispatchMessage(&msg); }
    return 0;
}
