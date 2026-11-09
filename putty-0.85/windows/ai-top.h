#ifndef PUTTY_WINDOWS_AI_TOP_H
#define PUTTY_WINDOWS_AI_TOP_H

typedef struct AiTop AiTop;
AiTop *ai_top_start(WinGuiSeat *parent, AiTaskProgram program, const unsigned char *id,
                    const char *path, size_t path_len);
bool ai_top_can_send(const AiTop *top, AiTaskProgram program, const unsigned char *id);
bool ai_top_paste(AiTop *top, AiTaskProgram program, const unsigned char *id,
                  const char *text, size_t length);
bool ai_top_save(AiTop *top, const unsigned char *id, const char *path,
                 size_t path_len, bool commit);
bool ai_top_active(const AiTop *top);
bool ai_top_send(AiTop *top, AiTaskProgram program, const unsigned char *id, AiInputKind kind);
bool ai_top_focus(AiTop *top);
void ai_top_tick(AiTop *top);
void ai_top_destroy(AiTop **top);

#endif
