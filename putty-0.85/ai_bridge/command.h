#ifndef PUTTY_AI_COMMAND_H
#define PUTTY_AI_COMMAND_H
#include <stddef.h>
#define AI_COMMAND_MAX 2048
#define AI_INPUT_LINE_MAX (AI_COMMAND_MAX + 1)
#define AI_VI_PATH_MAX 512
#define AI_VI_FILE_COMMAND_MAX (AI_VI_PATH_MAX * 4 + 512)
int ai_command_validate(const char *, size_t);
int ai_text_validate(const char *, size_t);
int ai_file_path_validate(const char *, size_t);
int ai_task_file_command(const char *, size_t, char *, size_t);
size_t ai_command_input(const char *, size_t, char *, size_t);
#endif
