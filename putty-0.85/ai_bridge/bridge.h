#ifndef PUTTY_AI_BRIDGE_H
#define PUTTY_AI_BRIDGE_H
#include <windows.h>
#include <stddef.h>
typedef struct AiBridge AiBridge;
#define IDM_AI_OBSERVE 0x0300
#define IDM_AI_CONTROL 0x0310
#define IDM_AI_COMMAND 0x0320
#define IDM_AI_REVOKE  0x0330
#define WM_AI_BRIDGE (WM_APP + 71)
#define AI_TIMER 0x504149
#ifdef PUTTY_AI_BRIDGE
void ai_bridge_add_menu(HMENU);
void ai_bridge_menu(AiBridge **, HWND, HMENU, UINT, int,
                    int (*send)(void *, const char *, size_t), void *);
void ai_bridge_dispatch(AiBridge *, HWND, HMENU);
void ai_bridge_offer_pending(AiBridge **, HWND, HMENU, int,
                            int (*)(void *,const char *,size_t), void *);
void ai_bridge_capture(AiBridge *, const void *, size_t);
void ai_bridge_human_input(AiBridge *);
void ai_bridge_stop(AiBridge **);
int ai_bridge_is_menu(UINT);
int ai_bridge_access_state(AiBridge *);
void ai_bridge_title_prefix(AiBridge *, wchar_t *, size_t);
void ai_bridge_cancel_approval(HWND);
#else
#define ai_bridge_add_menu(m) ((void)0)
#define ai_bridge_menu(a,b,c,d,e,f,g) ((void)0)
#define ai_bridge_dispatch(a,b,c) ((void)0)
#define ai_bridge_offer_pending(a,b,c,d,e,f) ((void)0)
#define ai_bridge_capture(a,b,c) ((void)0)
#define ai_bridge_human_input(a) ((void)0)
#define ai_bridge_stop(a) ((void)0)
#define ai_bridge_is_menu(a) 0
#define ai_bridge_access_state(a) 0
#define ai_bridge_title_prefix(a,b,c) (*(b)=0)
#define ai_bridge_cancel_approval(a) ((void)0)
#endif
#endif

