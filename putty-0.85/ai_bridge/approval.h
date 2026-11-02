#ifndef PUTTY_AI_APPROVAL_H
#define PUTTY_AI_APPROVAL_H
#include <windows.h>
/* Returns immediately. Completion runs on the owning GUI thread. */
HWND ai_approval_open(HWND owner, const char *title, const char *text,
                      void (*complete)(void *, int), void *context);
#endif
