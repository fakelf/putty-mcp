#ifndef PUTTY_AI_COMMAND_H
#define PUTTY_AI_COMMAND_H
#include <stddef.h>
#define AI_COMMAND_MAX 2048
#define AI_INPUT_LINE_MAX (AI_COMMAND_MAX * 4 + 512)
int ai_command_validate(const char *, size_t);
int ai_command_wrap(const char *, const char *, char *, size_t);
#endif
