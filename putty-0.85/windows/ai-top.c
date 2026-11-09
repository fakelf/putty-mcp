/* The viewer and its SSH channel both live on PuTTY's event thread. */
#include <wchar.h>
#include "putty.h"
#include "ssh.h"
#include "ssh/channel.h"
#include "terminal.h"
#include "../ai_bridge/bridge.h"
#include "win-gui-seat.h"
#include "ai-top.h"

enum { TOP_MAX_COLS = 240, TOP_MAX_ROWS = 100 };
struct AiTop {
    Channel chan;
    SshChannel *ssh;
    TermWin termwin;
    Terminal *term;
    Conf *conf;
    struct unicode_data ucs;
    AiBridge *bridge;
    WinGuiSeat *parent;
    AiTaskProgram program;
    unsigned char id[16];
    char *file_command;
    char file_path[AI_VI_PATH_MAX];
    size_t file_path_len;
    AiTopState state;
    enum { WAIT_OPEN, WAIT_PTY, WAIT_EXEC, EXEC_STARTED } stage;
    bool input_wanted, eof, remote_close, taken_over, dirty, frame_dirty;
    bool exit_seen, signal_seen;
    int exit_detail;
    int start_error;
    ULONGLONG last_frame;
    HWND window;
    int width, height;
    int cursor_x, cursor_y;
    bool cursor_visible;
    wchar_t pending_surrogate;
    const wchar_t *display;
    size_t display_len;
};

static void top_publish(AiTop *top, AiTopState state, int detail)
{
    top->state = state;
    ai_bridge_top_state(top->bridge, top->id, state, detail, top->taken_over);
}

static void top_close_local(AiTop *top)
{
    if (top->ssh)
        sshfwd_initiate_close(top->ssh, NULL);
}

static void top_free(Channel *chan)
{
    AiTop *top = container_of(chan, AiTop, chan);
    top->ssh = NULL;
    ai_top_tick(top);
    if (top->start_error) {
        top_publish(top, AI_TOP_FAILED, top->start_error);
        return;
    }
    if (top->remote_close && top->exit_seen)
        top_publish(top, AI_TOP_EXITED, top->exit_detail);
    else if (top->remote_close && top->signal_seen)
        top_publish(top, AI_TOP_SIGNALLED, 0);
    else if (top->remote_close)
        top_publish(top, AI_TOP_CLOSED, 0);
    else
        top_publish(top, AI_TOP_LOST, 0);
}

static void top_open_confirmation(Channel *chan)
{
    AiTop *top = container_of(chan, AiTop, chan);
    top->stage = WAIT_PTY;
    sshfwd_request_pty(top->ssh, true, top->conf, top->term->cols, top->term->rows);
}

static void top_open_failed(Channel *chan, const char *err)
{
    AiTop *top = container_of(chan, AiTop, chan);
    top->start_error = 1;
}

static void top_request_response(Channel *chan, bool success)
{
    AiTop *top = container_of(chan, AiTop, chan);
    if (!success) {
        top->start_error = top->stage == WAIT_PTY ? 2 : 3;
        /* Defer close until the SSH request callback has returned. */
        return;
    }
    if (top->stage == WAIT_PTY) {
        if (top->remote_close || top->eof)
            return;
        top->stage = WAIT_EXEC;
        const char *command = top->file_command ? top->file_command : ai_task_command(top->program);
        sshfwd_start_command(top->ssh, true, command);
    } else if (top->stage == WAIT_EXEC && !top->remote_close && !top->eof) {
        top->stage = EXEC_STARTED;
        top_publish(top, AI_TOP_RUNNING, 0);
    }
}

static size_t top_data(Channel *chan, bool is_stderr, const void *data, size_t len)
{
    AiTop *top = container_of(chan, AiTop, chan);
    top->dirty = true;
    return term_data(top->term, data, len);
}

static void top_eof(Channel *chan)
{
    AiTop *top = container_of(chan, AiTop, chan);
    top->eof = true;
    sshfwd_write_eof(top->ssh);
}

static void top_remote_close(Channel *chan)
{
    container_of(chan, AiTop, chan)->remote_close = true;
}

static void top_input_wanted(Channel *chan, bool wanted)
{
    container_of(chan, AiTop, chan)->input_wanted = wanted;
}

static bool top_exit_status(Channel *chan, int status)
{
    AiTop *top = container_of(chan, AiTop, chan);
    top->exit_seen = true;
    top->exit_detail = status;
    top_publish(top, AI_TOP_EXIT_STATUS_RECEIVED, status);
    return true;
}

static bool top_exit_signal(Channel *chan, ptrlen name, bool core, ptrlen msg)
{
    AiTop *top = container_of(chan, AiTop, chan);
    top->signal_seen = true;
    top_publish(top, AI_TOP_EXIT_SIGNAL_RECEIVED, 0);
    return true;
}

static bool top_exit_signal_numeric(Channel *chan, int signal, bool core, ptrlen msg)
{
    return top_exit_signal(chan, PTRLEN_LITERAL(""), core, msg);
}

static char *top_close_log(Channel *chan)
{
    AiTop *top = container_of(chan, AiTop, chan);
    return dupstr(ai_task_is_vi(top->program) ?
                  "Interactive vi channel closed" : "Interactive top channel closed");
}

static const ChannelVtable top_channel_vt = {
    .free = top_free,
    .open_confirmation = top_open_confirmation,
    .open_failed = top_open_failed,
    .send = top_data,
    .send_eof = top_eof,
    .remote_close = top_remote_close,
    .set_input_wanted = top_input_wanted,
    .log_close_msg = top_close_log,
    .want_close = chan_default_want_close,
    .rcvd_exit_status = top_exit_status,
    .rcvd_exit_signal = top_exit_signal,
    .rcvd_exit_signal_numeric = top_exit_signal_numeric,
    .run_shell = chan_no_run_shell,
    .run_command = chan_no_run_command,
    .run_subsystem = chan_no_run_subsystem,
    .enable_x11_forwarding = chan_no_enable_x11_forwarding,
    .enable_agent_forwarding = chan_no_enable_agent_forwarding,
    .allocate_pty = chan_no_allocate_pty,
    .set_env = chan_no_set_env,
    .send_break = chan_no_send_break,
    .send_signal = chan_no_send_signal,
    .change_window_size = chan_no_change_window_size,
    .request_response = top_request_response,
};

static bool view_no_draw(TermWin *win) { return false; }
static void view_bool(TermWin *win, bool flag) {}
static void view_void(TermWin *win) {}
static void view_int(TermWin *win, int value) {}
static void view_position(TermWin *win, int x, int y) {}
static void view_scroll(TermWin *win, int total, int start, int page) {}
static void view_title(TermWin *win, const char *text, int codepage) {}
static void view_palette(TermWin *win, unsigned start, unsigned count, const rgb *colours) {}
static void view_overrides(TermWin *win, Terminal *term) {}
static void view_clip(TermWin *win, int clip, wchar_t *text, int *attrs,
                      truecolour *colour, int len, bool deselect) {}
static void view_resize(TermWin *win, int cols, int rows)
{
    term_resize_request_completed(container_of(win, AiTop, termwin)->term);
}
static void view_unthrottle(TermWin *win, size_t backlog)
{
    AiTop *top = container_of(win, AiTop, termwin);
    if (top->ssh)
        sshfwd_unthrottle(top->ssh, backlog);
}

static const TermWinVtable top_termwin_vt = {
    .setup_draw_ctx = view_no_draw,
    .set_cursor_pos = view_position,
    .set_raw_mouse_mode = view_bool,
    .set_raw_mouse_mode_pointer = view_bool,
    .set_scrollbar = view_scroll,
    .bell = view_int,
    .clip_write = view_clip,
    .clip_request_paste = view_int,
    .refresh = view_void,
    .request_resize = view_resize,
    .set_title = view_title,
    .set_icon_title = view_title,
    .set_minimised = view_bool,
    .set_maximised = view_bool,
    .move = view_position,
    .set_zorder = view_bool,
    .palette_set = view_palette,
    .palette_get_overrides = view_overrides,
    .unthrottle = view_unthrottle,
};

static bool top_ready(const AiTop *top)
{
    return top && top->ssh && top->state == AI_TOP_RUNNING && top->input_wanted &&
        !top->eof && !top->remote_close && !top->exit_seen && !top->signal_seen;
}

bool ai_top_can_send(const AiTop *top, AiTaskProgram program, const unsigned char *id)
{
    return top_ready(top) && top->program == program && !top->taken_over &&
        !memcmp(top->id, id, 16);
}

static bool top_can_paste(const AiTop *top)
{
    return top_ready(top) && ai_task_is_vi(top->program) && !top->taken_over &&
        top->term->bracketed_paste && !top->term->no_bracketed_paste;
}

bool ai_top_paste(AiTop *top, AiTaskProgram program, const unsigned char *id,
                  const char *text, size_t length)
{
    static const char begin[] = "\033[200~", end[] = "\033[201~";
    if (!top_can_paste(top) || top->program != program || memcmp(top->id, id, 16) ||
        length > AI_VI_TEXT_MAX)
        return false;
    if (!length)
        return true;

    /* Queue one complete frame. Never fall back to typing unframed text. */
    char input[sizeof(begin) - 1 + AI_VI_TEXT_MAX + sizeof(end) - 1];
    memcpy(input, begin, sizeof(begin) - 1);
    memcpy(input + sizeof(begin) - 1, text, length);
    memcpy(input + sizeof(begin) - 1 + length, end, sizeof(end) - 1);
    sshfwd_write(top->ssh, input, sizeof(begin) - 1 + length + sizeof(end) - 1);
    SecureZeroMemory(input, sizeof(input));
    return true;
}

bool ai_top_save(AiTop *top, const unsigned char *id, const char *path,
                 size_t path_len, bool commit)
{
    if (!ai_top_can_send(top, AI_PROGRAM_VI_FILE, id) || !top_can_paste(top) ||
        !path || !path_len || path_len != top->file_path_len ||
        memcmp(top->file_path, path, path_len))
        return false;

    /* The Vim profile starts with 'write' off. Open that gate only around
     * this guarded, unforced write of the original buffer. */
    static const char command[] =
        "\033:if exists('g:putty_mcp_path') && bufnr('%') == g:putty_mcp_buf && "
        "expand('%:p') ==# g:putty_mcp_path && "
        "resolve(g:putty_mcp_path) ==# g:putty_mcp_resolved && "
        "getftype(g:putty_mcp_path) ==# 'file' && "
        "getftime(g:putty_mcp_path) == g:putty_mcp_mtime && "
        "getfsize(g:putty_mcp_path) == g:putty_mcp_size | "
        "try | set write | write | "
        "let g:putty_mcp_mtime = getftime(g:putty_mcp_path) | "
        "let g:putty_mcp_size = getfsize(g:putty_mcp_path) | "
        "finally | set nowrite | endtry | else | "
        "echoerr 'PuTTY MCP: file changed or missing, or current buffer is not the approved file' | endif\r";
    if (commit)
        sshfwd_write(top->ssh, command, sizeof(command) - 1);
    return true;
}

bool ai_top_active(const AiTop *top) { return top && top->ssh; }

static const char *top_key_sequence(AiTop *top, AiInputKind kind, char *buffer)
{
    if (kind == AI_INPUT_KEY_BACKSPACE)
        return top->term->bksp_is_delete ? "\177" : "\b";
    if (kind >= AI_INPUT_KEY_UP && kind <= AI_INPUT_KEY_END) {
        static const char cursor_keys[] = "ABDCHF";
        format_arrow_key(buffer, top->term, cursor_keys[kind - AI_INPUT_KEY_UP],
                         false, false, false, NULL);
        return buffer;
    }
    const AiSpecialKey *key = ai_task_special_key(kind);
    return key ? key->sequence : NULL;
}

bool ai_top_send(AiTop *top, AiTaskProgram program, const unsigned char *id, AiInputKind kind)
{
    if (!ai_top_can_send(top, program, id) ||
        (kind >= AI_INPUT_KEY_UNDO && kind <= AI_INPUT_KEY_DISCARD_AND_QUIT &&
         !ai_task_is_vi(program)))
        return false;
    const char *sequence;
    char buffer[32];
    if (kind == AI_INPUT_TASK_QUIT)
        sequence = ai_task_is_vi(program) ? "\033:qa\r" : "q";
    else if (kind == AI_INPUT_TASK_INTERRUPT)
        sequence = "\003";
    else {
        sequence = top_key_sequence(top, kind, buffer);
        if (!sequence)
            return false;
    }
    sshfwd_write(top->ssh, sequence, strlen(sequence));
    return true;
}

static void top_manual_input(AiTop *top, const char *sequence, size_t length)
{
    if (!top_ready(top))
        return;
    if (!top->taken_over) {
        top->taken_over = true;
        ai_bridge_human_input(top->bridge);
        top_publish(top, AI_TOP_RUNNING, 0);
    }
    sshfwd_write(top->ssh, sequence, length);
}

static void top_manual_key(AiTop *top, AiInputKind kind)
{
    if (!top_ready(top))
        return;
    top->pending_surrogate = 0;
    char buffer[32];
    const char *sequence = top_key_sequence(top, kind, buffer);
    if (sequence)
        top_manual_input(top, sequence, strlen(sequence));
}

static void top_manual_char(AiTop *top, wchar_t ch)
{
    if (!top_ready(top))
        return;
    switch (ch) {
      case L'\b': top_manual_key(top, AI_INPUT_KEY_BACKSPACE); return;
      case L'\t':
        top_manual_key(top, GetKeyState(VK_SHIFT) < 0 ? AI_INPUT_KEY_SHIFT_TAB : AI_INPUT_KEY_TAB);
        return;
      case L'\r': top_manual_key(top, AI_INPUT_KEY_ENTER); return;
    }

    if (ch >= 0xd800 && ch <= 0xdbff) {
        top->pending_surrogate = ch;
        return;
    }
    wchar_t units[2] = {ch, 0};
    int count = 1;
    if (ch >= 0xdc00 && ch <= 0xdfff) {
        if (!top->pending_surrogate)
            return;
        units[0] = top->pending_surrogate;
        units[1] = ch;
        count = 2;
    }
    top->pending_surrogate = 0;
    /* These WM_CHAR events come from the user after taking over the task. */
    if (ch && ch != 0x7f) {
        char encoded[4];
        int length = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS,
                                         units, count, encoded, sizeof(encoded), NULL, NULL);
        if (length > 0)
            top_manual_input(top, encoded, (size_t)length);
    }
}

static void paint_top(AiTop *top, HWND hwnd)
{
    const wchar_t *opening = ai_task_is_vi(top->program) ?
        L"Opening vi on a separate SSH channel..." : L"Opening top on a separate SSH channel...";
    WinGuiSeat *parent = top->parent;
    PAINTSTRUCT ps;
    HDC dc = BeginPaint(hwnd, &ps);
    SetDCBrushColor(dc, parent->colours[OSC4_COLOUR_bg]);
    FillRect(dc, &ps.rcPaint, (HBRUSH)GetStockObject(DC_BRUSH));
    SetTextColor(dc, parent->colours[OSC4_COLOUR_fg]);
    SetBkMode(dc, TRANSPARENT);
    HGDIOBJ old_font = SelectObject(dc, parent->fonts[FONT_NORMAL]);
    const wchar_t *line = top->display ? top->display : opening;
    size_t remaining = top->display ? top->display_len : wcslen(opening);
    int y = parent->offset_height;
    while (remaining && y < ps.rcPaint.bottom) {
        const wchar_t *newline = wmemchr(line, L'\n', remaining);
        size_t count = newline ? (size_t)(newline - line) : remaining;
        size_t chars = count - (count && line[count - 1] == L'\r');
        if (chars && y + parent->font_height > ps.rcPaint.top)
            TextOutW(dc, parent->offset_width, y, line, (int)chars);
        if (!newline)
            break;
        line = newline + 1;
        remaining -= count + 1;
        y += parent->font_height;
    }
    if (top->cursor_visible && GetFocus() == hwnd) {
        int left = parent->offset_width + top->cursor_x * parent->font_width;
        int top_edge = parent->offset_height + top->cursor_y * parent->font_height;
        RECT cursor = {left, top_edge, left + parent->font_width,
                       top_edge + parent->font_height};
        SetDCBrushColor(dc, parent->colours[OSC4_COLOUR_cursor_bg]);
        FrameRect(dc, &cursor, (HBRUSH)GetStockObject(DC_BRUSH));
    }
    SelectObject(dc, old_font);
    EndPaint(hwnd, &ps);
}

static LRESULT CALLBACK top_window(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    AiTop *top = (AiTop *)GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if (msg == WM_NCCREATE) {
        top = ((CREATESTRUCTW *)lp)->lpCreateParams;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (LONG_PTR)top);
    } else if (msg == WM_PAINT && top) {
        paint_top(top, hwnd);
        return 0;
    } else if (msg == WM_ERASEBKGND) {
        return 1;
    } else if (msg == WM_KEYDOWN &&
               GetKeyState(VK_CONTROL) >= 0 && GetKeyState(VK_SHIFT) >= 0) {
        AiInputKind kind;
        if (wp >= VK_F1 && wp <= VK_F12) {
            kind = (AiInputKind)(AI_INPUT_KEY_F1 + wp - VK_F1);
        } else switch (wp) {
          case VK_DELETE: kind = AI_INPUT_KEY_DELETE; break;
          case VK_UP: kind = AI_INPUT_KEY_UP; break;
          case VK_DOWN: kind = AI_INPUT_KEY_DOWN; break;
          case VK_LEFT: kind = AI_INPUT_KEY_LEFT; break;
          case VK_RIGHT: kind = AI_INPUT_KEY_RIGHT; break;
          case VK_HOME: kind = AI_INPUT_KEY_HOME; break;
          case VK_END: kind = AI_INPUT_KEY_END; break;
          case VK_PRIOR: kind = AI_INPUT_KEY_PAGE_UP; break;
          case VK_NEXT: kind = AI_INPUT_KEY_PAGE_DOWN; break;
          case VK_INSERT: kind = AI_INPUT_KEY_INSERT; break;
          default: return DefWindowProcW(hwnd, msg, wp, lp);
        }
        top_manual_key(top, kind);
        return 0;
    } else if (msg == WM_CHAR) {
        top_manual_char(top, (wchar_t)wp);
        return 0;
    } else if ((msg == WM_SETFOCUS || msg == WM_KILLFOCUS) && top) {
        if (msg == WM_KILLFOCUS)
            top->pending_surrogate = 0;
        InvalidateRect(hwnd, NULL, FALSE);
    } else if (msg == WM_LBUTTONDOWN) {
        SetFocus(hwnd);
        return 0;
    } else if (msg == WM_MOUSEWHEEL) {
        return 0;
    } else if (msg == WM_CLOSE) {
        top_close_local(top);
        return 0;
    } else if (msg == WM_NCDESTROY && top) {
        top->window = NULL;
    }
    return DefWindowProcW(hwnd, msg, wp, lp);
}

static void top_layout(AiTop *top)
{
    RECT rect;
    WinGuiSeat *parent = top->parent;
    if (GetClientRect(parent->term_hwnd, &rect) &&
        (rect.right != top->width || rect.bottom != top->height)) {
        top->width = rect.right;
        top->height = rect.bottom;
        if (top->window)
            SetWindowPos(top->window, NULL, 0, 0, top->width, top->height,
                         SWP_NOACTIVATE | SWP_NOZORDER);
    }
    int cols = max(1, min(parent->term->cols, TOP_MAX_COLS));
    int rows = max(1, min(parent->term->rows, TOP_MAX_ROWS));
    if (cols != top->term->cols || rows != top->term->rows) {
        term_size(top->term, rows, cols, 0);
        if (top->display)
            top->dirty = true;
        if (top->ssh && top->stage != WAIT_OPEN && !top->eof &&
            !top->remote_close && !top->exit_seen && !top->signal_seen)
            sshfwd_send_terminal_size_change(top->ssh, cols, rows);
    }
}

bool ai_top_focus(AiTop *top)
{
    if (!top || !top->window || !IsWindowVisible(top->window))
        return false;
    SetFocus(top->window);
    return true;
}

static void top_hide(AiTop *top)
{
    if (!top->window)
        return;
    HWND parent = top->parent->term_hwnd;
    bool restore_focus = GetFocus() == top->window;
    HWND view = top->window;
    top->window = NULL;
    DestroyWindow(view);
    if (restore_focus)
        SetFocus(parent);
    term_invalidate(top->parent->term);
    InvalidateRect(parent, NULL, TRUE);
}

AiTop *ai_top_start(WinGuiSeat *parent, AiTaskProgram program, const unsigned char *id,
                    const char *path, size_t path_len)
{
    ConnectionLayer *connection = ssh_task_connection(parent->backend, NULL, NULL, NULL);
    if (!connection || !id)
        return NULL;
    char file_command[AI_VI_FILE_COMMAND_MAX];
    if (program == AI_PROGRAM_VI_FILE) {
        if (!path || !path_len || path_len > AI_VI_PATH_MAX ||
            !ai_task_file_command(path, path_len, file_command, sizeof(file_command)))
            return NULL;
    } else if (!ai_task_command(program) || path_len) {
        return NULL;
    }
    WNDCLASSW wc = {0};
    wc.lpfnWndProc = top_window;
    wc.hInstance = GetModuleHandle(NULL);
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);
    wc.lpszClassName = L"PuTTY AI top task";
    if (!RegisterClassW(&wc) && GetLastError() != ERROR_CLASS_ALREADY_EXISTS)
        return NULL;

    AiTop *top = snew(AiTop);
    memset(top, 0, sizeof(*top));
    top->chan.vt = &top_channel_vt;
    top->termwin.vt = &top_termwin_vt;
    top->parent = parent;
    top->program = program;
    top->conf = conf_copy(parent->conf);
    top->bridge = parent->ai_bridge;
    memcpy(top->id, id, 16);
    if (program == AI_PROGRAM_VI_FILE) {
        top->file_command = dupstr(file_command);
        memcpy(top->file_path, path, path_len);
        top->file_path_len = path_len;
    }
    conf_set_str(top->conf, CONF_termtype, "xterm");
    conf_set_str(top->conf, CONF_line_codepage, "UTF-8");
    init_ucs_generic(top->conf, &top->ucs);
    top->term = term_init(top->conf, &top->ucs, &top->termwin);
    top_layout(top);
    term_set_trust_status(top->term, false);
    top->window = CreateWindowExW(0, wc.lpszClassName,
        ai_task_is_vi(program) ? L"vi" : L"top", WS_CHILD,
        0, 0, top->width, top->height, parent->term_hwnd, NULL, wc.hInstance, top);
    if (!top->window) {
        ai_top_destroy(&top);
        return NULL;
    }
    top->input_wanted = true;
    top->ssh = ssh_session_open(connection, &top->chan);
    top_publish(top, AI_TOP_OPENING, 0);
    ShowWindow(top->window, SW_SHOWNOACTIVATE);
    ai_top_focus(top);
    return top;
}

void ai_top_tick(AiTop *top)
{
    if (!top)
        return;
    if (top->start_error && top->ssh)
        top_close_local(top);
    if (top->ssh)
        top_layout(top);
    else
        top_hide(top);
    if (top->dirty) {
        top->dirty = false;
        top->frame_dirty = true;
        top->display = term_copy_visible(top->term, &top->display_len);
        term_get_cursor_position(top->term, &top->cursor_x, &top->cursor_y);
        top->cursor_visible = top->stage == EXEC_STARTED && top->term->cursor_on;
        if (top->window)
            InvalidateRect(top->window, NULL, FALSE);
    }
    ULONGLONG now = GetTickCount64();
    if (!top->frame_dirty || now - top->last_frame < 1000)
        return;
    top->frame_dirty = false;
    top->last_frame = now;
    size_t length = top->display_len;
    const wchar_t *display = top->display;
    char utf8[AI_TOP_SCREEN_MAX * 4];
    size_t encode_length = length < AI_TOP_SCREEN_MAX ? length : AI_TOP_SCREEN_MAX;
    if (encode_length < length && display[encode_length-1] >= 0xd800 &&
        display[encode_length-1] <= 0xdbff)
        --encode_length;
    int size = WideCharToMultiByte(CP_UTF8, 0, display, (int)encode_length, utf8,
                                   sizeof(utf8), NULL, NULL);
    if (size > 0 && !top->taken_over) {
        size_t used = (size_t)size;
        if (used > AI_TOP_SCREEN_MAX) {
            used = AI_TOP_SCREEN_MAX;
            while ((utf8[used] & 0xC0) == 0x80)
                --used;
        }
        ai_bridge_top_screen(top->bridge, top->id, utf8, used,
                             used < (size_t)size || encode_length < length,
                             top->cursor_visible ? top->cursor_x : -1,
                             top->cursor_visible ? top->cursor_y : -1,
                             top_can_paste(top));
    }
}

void ai_top_destroy(AiTop **owner)
{
    AiTop *top = *owner;
    if (!top)
        return;
    *owner = NULL;
    top_close_local(top);
    top_hide(top);
    term_free(top->term);
    conf_free(top->conf);
    sfree(top->file_command);
    sfree(top);
}
