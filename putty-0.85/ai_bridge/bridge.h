#ifndef PUTTY_AI_BRIDGE_H
#define PUTTY_AI_BRIDGE_H
#include <windows.h>
#include <stddef.h>
#include "command.h"
typedef struct AiBridge AiBridge;
typedef enum AiTaskProgram {
    AI_PROGRAM_NONE, AI_PROGRAM_TOP, AI_PROGRAM_VI, AI_PROGRAM_VI_FILE
} AiTaskProgram;
typedef enum AiInputKind {
    AI_INPUT_SCRIPT, AI_INPUT_INTERRUPT, AI_INPUT_TASK_START,
    AI_INPUT_TASK_QUIT, AI_INPUT_TASK_INTERRUPT, AI_INPUT_VI_TEXT,
    AI_INPUT_VI_SAVE_CHECK, AI_INPUT_VI_SAVE,
    AI_INPUT_CHILD_OPEN, AI_INPUT_CHILD_CLOSE,
    AI_INPUT_KEY_ESCAPE, AI_INPUT_KEY_F1, AI_INPUT_KEY_F2, AI_INPUT_KEY_F3,
    AI_INPUT_KEY_F4, AI_INPUT_KEY_F5, AI_INPUT_KEY_F6, AI_INPUT_KEY_F7,
    AI_INPUT_KEY_F8, AI_INPUT_KEY_F9, AI_INPUT_KEY_F10, AI_INPUT_KEY_F11,
    AI_INPUT_KEY_F12, AI_INPUT_KEY_CTRL_Q, AI_INPUT_KEY_CTRL_S,
    AI_INPUT_KEY_CTRL_D, AI_INPUT_KEY_CTRL_Z,
    AI_INPUT_KEY_BACKSPACE, AI_INPUT_KEY_DELETE, AI_INPUT_KEY_UP,
    AI_INPUT_KEY_DOWN, AI_INPUT_KEY_LEFT, AI_INPUT_KEY_RIGHT, AI_INPUT_KEY_HOME,
    AI_INPUT_KEY_END, AI_INPUT_KEY_PAGE_UP, AI_INPUT_KEY_PAGE_DOWN,
    AI_INPUT_KEY_INSERT, AI_INPUT_KEY_TAB, AI_INPUT_KEY_SHIFT_TAB,
    AI_INPUT_KEY_ENTER, AI_INPUT_KEY_UNDO, AI_INPUT_KEY_REDO,
    AI_INPUT_KEY_INSERT_MODE, AI_INPUT_KEY_DISCARD_AND_QUIT,
    AI_INPUT_ATTACH, AI_INPUT_KEY, AI_INPUT_TEXT, AI_INPUT_PASTE,
    AI_INPUT_SCREEN_REFRESH
} AiInputKind;
typedef struct AiSpecialKey {
    const char *name, *sequence;
} AiSpecialKey;
typedef enum AiTopState {
    AI_TOP_OPENING = 1, AI_TOP_RUNNING, AI_TOP_EXITED, AI_TOP_SIGNALLED,
    AI_TOP_CLOSED, AI_TOP_FAILED, AI_TOP_LOST,
    AI_TOP_EXIT_STATUS_RECEIVED, AI_TOP_EXIT_SIGNAL_RECEIVED
} AiTopState;
#define AI_TOP_SCREEN_MAX 8192
#define AI_VI_TEXT_MAX 1024
#define AI_COMMAND_PROMPT_MAX 256
#define AI_TERMINAL_SCREEN_MAX 8192
#define AI_TERMINAL_TEXT_MAX 1024
#define AI_TERMINAL_KEY_MAX 64
typedef int (*AiSend)(void *, AiInputKind, AiTaskProgram, const char *, size_t);
#define IDM_AI_OBSERVE 0x0300
#define WM_AI_BRIDGE (WM_APP + 71)
#define WM_AI_CLOSE_CHILD (WM_APP + 72)
#define AI_TIMER 0x504149
#ifdef PUTTY_AI_BRIDGE
const char *ai_task_command(AiTaskProgram);
int ai_task_is_vi(AiTaskProgram);
const AiSpecialKey *ai_task_special_key(AiInputKind);
void ai_bridge_add_menu(HMENU);
void ai_bridge_toggle_observation(AiBridge **, HWND, HMENU, AiSend, void *);
void ai_bridge_dispatch(AiBridge *, HWND, HMENU);
void ai_bridge_offer_pending(AiBridge **, HWND, HMENU, AiSend, void *);
void ai_bridge_capture(AiBridge *, const void *, size_t);
void ai_bridge_command_prompt(AiBridge *, const char *, size_t);
int ai_bridge_terminal_active(AiBridge *);
void ai_bridge_terminal_screen(AiBridge *, const char *, size_t,
                               int columns, int rows, int cursor_x, int cursor_y,
                               unsigned flags);
void ai_bridge_human_input(AiBridge *);
void ai_bridge_stop(AiBridge **);
int ai_bridge_access_state(AiBridge *);
void ai_bridge_title_prefix(AiBridge *, wchar_t *, size_t);
void ai_bridge_cancel_approval(HWND);
void ai_bridge_top_target(AiBridge *, const char *target);
AiBridge *ai_bridge_start_child(HWND, const AiBridge *parent, const unsigned char id[16]);
void ai_bridge_child_mode(AiBridge *, int enabled);
void ai_bridge_child_failed(AiBridge *, const unsigned char id[16], unsigned stage);
void ai_bridge_child_closed(AiBridge *, const unsigned char id[16]);
void ai_bridge_top_state(AiBridge *, const unsigned char *id, AiTopState state,
                         int detail, int taken_over);
void ai_bridge_top_screen(AiBridge *, const unsigned char *id, const char *text,
                          size_t len, int truncated, int cursor_column, int cursor_row,
                          int paste_available);
#else
#define ai_bridge_add_menu(m) ((void)0)
#define ai_bridge_dispatch(a,b,c) ((void)0)
#define ai_bridge_offer_pending(a,b,c,d,e) ((void)0)
#define ai_bridge_capture(a,b,c) ((void)0)
#define ai_bridge_human_input(a) ((void)0)
#define ai_bridge_stop(a) ((void)0)
#define ai_bridge_access_state(a) 0
#define ai_bridge_title_prefix(a,b,c) (*(b)=0)
#define ai_bridge_cancel_approval(a) ((void)0)
#endif
#endif

