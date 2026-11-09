#ifndef PUTTY_WINDOWS_AI_CHILD_H
#define PUTTY_WINDOWS_AI_CHILD_H

Backend *ai_child_backend_new(WinGuiSeat *window);
void ai_child_backend_open(Backend *backend, ConnectionLayer *connection);
void ai_child_backend_close(Backend *backend);
void win_ai_child_connected(WinGuiSeat *window);
void win_ai_child_disconnected(WinGuiSeat *window, unsigned startup_stage);
RECT ai_child_placement(RECT work, const RECT *occupied, size_t count,
                        int width, int height, int caption_height);

#endif
