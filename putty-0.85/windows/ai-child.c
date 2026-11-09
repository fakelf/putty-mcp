/* A Windows terminal backend attached to one channel of an existing SSH-2 connection. */
#include "putty.h"
#include "ssh.h"
#include "ssh/channel.h"
#include "terminal.h"
#include "win-gui-seat.h"
#include "ai-child.h"

static int compare_coordinate(const void *left, const void *right)
{
    LONG a = *(const LONG *)left, b = *(const LONG *)right;
    return (a > b) - (a < b);
}

static int64_t child_overlap(RECT position, const RECT *occupied, size_t count,
                             int title_height)
{
    int64_t area = 0;
    for (size_t i = 0; i < count; ++i) {
        RECT target = occupied[i], overlap;
        if (title_height)
            target.bottom = min(target.bottom, target.top + title_height);
        if (IntersectRect(&overlap, &position, &target))
            area += (int64_t)(overlap.right - overlap.left) * (overlap.bottom - overlap.top);
    }
    return area;
}

RECT ai_child_placement(RECT work, const RECT *occupied, size_t count,
                        int width, int height, int caption_height)
{
    enum { MAX_EDGES = 2 + 2 * (AI_CHILD_LIMIT + 1) };
    int work_width = work.right - work.left, work_height = work.bottom - work.top;
    int step_x = min(caption_height, work_width / (3 * AI_CHILD_LIMIT));
    int step_y = min(caption_height, work_height / (3 * AI_CHILD_LIMIT));
    width = min(width, work_width - AI_CHILD_LIMIT * step_x);
    height = min(height, work_height - AI_CHILD_LIMIT * step_y);

    LONG rightmost = work.right - width, bottommost = work.bottom - height;
    /* Prefer aligned cells, since an offset free spot can fragment space
     * that would otherwise fit another child. */
    for (LONG y = work.top; y <= bottommost; y += height) {
        for (LONG x = work.left; x <= rightmost; x += width) {
            RECT position = {x, y, x + width, y + height};
            if (!child_overlap(position, occupied, count, 0))
                return position;
        }
    }

    /* A free rectangle can slide to the monitor or a neighbour's edge. Try
     * those boundaries in screen order before accepting any overlap. */
    LONG xs[MAX_EDGES] = {work.left, rightmost};
    LONG ys[MAX_EDGES] = {work.top, bottommost};
    size_t edges = 2;
    assert(count <= AI_CHILD_LIMIT + 1);
    for (size_t i = 0; i < count; ++i) {
        xs[edges] = max(work.left, min(rightmost, occupied[i].right));
        ys[edges++] = max(work.top, min(bottommost, occupied[i].bottom));
        xs[edges] = max(work.left, min(rightmost, occupied[i].left - width));
        ys[edges++] = max(work.top, min(bottommost, occupied[i].top - height));
    }
    qsort(xs, edges, sizeof(*xs), compare_coordinate);
    qsort(ys, edges, sizeof(*ys), compare_coordinate);

    RECT best = {0};
    int64_t best_title = INT64_MAX, best_overlap = INT64_MAX;
    for (size_t y = 0; y < edges; ++y) {
        for (size_t x = 0; x < edges; ++x) {
            RECT position = {xs[x], ys[y], xs[x] + width, ys[y] + height};
            int64_t overlap = child_overlap(position, occupied, count, 0);
            if (!overlap)
                return position;
            int64_t title = child_overlap(position, occupied, count, caption_height);
            if (title < best_title || (title == best_title && overlap < best_overlap)) {
                best = position;
                best_title = title;
                best_overlap = overlap;
            }
        }
    }
    if (!best_title && (width <= work_width / 2 || height <= work_height / 2))
        return best;

    /* Reserve the first usable diagonal slot, so subsequent openings have
     * another step available. If the monitor is too small, preserve as much
     * of the existing title bars as possible. */
    for (int slot = 0; slot <= AI_CHILD_LIMIT; ++slot) {
        LONG x = work.left + slot * step_x, y = work.top + slot * step_y;
        RECT position = {x, y, x + width, y + height};
        int64_t title = child_overlap(position, occupied, count, caption_height);
        if (!title)
            return position;
        int64_t overlap = child_overlap(position, occupied, count, 0);
        if (title < best_title || (title == best_title && overlap < best_overlap)) {
            best = position;
            best_title = title;
            best_overlap = overlap;
        }
    }
    return best;
}

typedef struct AiChild {
    Backend backend;
    Channel chan;
    SshChannel *ssh;
    WinGuiSeat *window;
    Ldisc *ldisc;
    enum { CHILD_READY, CHILD_WAIT_OPEN, CHILD_WAIT_PTY, CHILD_WAIT_SHELL } stage;
    bool input_wanted, eof, remote_close, closing, start_failed;
    int exit_code;
} AiChild;

static bool child_can_send(const AiChild *child)
{
    return child->ssh && child->stage == CHILD_READY && child->input_wanted &&
        !child->eof && !child->remote_close && !child->closing;
}

static void child_backend_send(Backend *backend, const char *data, size_t len)
{
    AiChild *child = container_of(backend, AiChild, backend);
    if (child_can_send(child))
        sshfwd_write(child->ssh, data, len);
}

static size_t child_backend_sendbuffer(Backend *backend)
{
    AiChild *child = container_of(backend, AiChild, backend);
    /* A zero-length write reports this channel's current backlog. */
    return child_can_send(child) ? sshfwd_write(child->ssh, "", 0) : 0;
}

static void child_backend_size(Backend *backend, int cols, int rows)
{
    AiChild *child = container_of(backend, AiChild, backend);
    if (child->ssh && (child->stage == CHILD_READY || child->stage == CHILD_WAIT_SHELL) &&
        !child->eof && !child->remote_close && !child->closing)
        sshfwd_send_terminal_size_change(child->ssh, cols, rows);
}

static void child_backend_special(Backend *backend, SessionSpecialCode code, int arg)
{
    AiChild *child = container_of(backend, AiChild, backend);
    if (!child_can_send(child))
        return;
    if (code == SS_EOF) {
        child->eof = true;
        sshfwd_write_eof(child->ssh);
    } else if (code == SS_BRK) {
        sshfwd_send_serial_break(child->ssh, false, 0);
    }
}

static bool child_backend_connected(Backend *backend)
{
    AiChild *child = container_of(backend, AiChild, backend);
    return child->ssh != NULL && !child->closing;
}

static int child_backend_exitcode(Backend *backend)
{
    AiChild *child = container_of(backend, AiChild, backend);
    return child->exit_code;
}

static bool child_backend_sendok(Backend *backend)
{
    return child_can_send(container_of(backend, AiChild, backend));
}

static bool child_backend_ldisc_option(Backend *backend, int option)
{
    return false; /* The remote PTY supplies echo and line editing. */
}

static void child_backend_provide_ldisc(Backend *backend, Ldisc *ldisc)
{
    container_of(backend, AiChild, backend)->ldisc = ldisc;
}

static void child_backend_unthrottle(Backend *backend, size_t backlog)
{
    AiChild *child = container_of(backend, AiChild, backend);
    if (child->ssh && !child->closing)
        sshfwd_unthrottle(child->ssh, backlog);
}

void ai_child_backend_close(Backend *backend)
{
    AiChild *child = container_of(backend, AiChild, backend);
    child->closing = true;
    if (child->ssh)
        sshfwd_initiate_close(child->ssh, NULL);
}

static void child_backend_free(Backend *backend)
{
    AiChild *child = container_of(backend, AiChild, backend);
    delete_callbacks_for_context(child);
    ai_child_backend_close(backend);
    sfree(child);
}

static const BackendVtable child_backend_vt = {
    .free = child_backend_free,
    .send = child_backend_send,
    .sendbuffer = child_backend_sendbuffer,
    .size = child_backend_size,
    .special = child_backend_special,
    .connected = child_backend_connected,
    .exitcode = child_backend_exitcode,
    .sendok = child_backend_sendok,
    .ldisc_option_state = child_backend_ldisc_option,
    .provide_ldisc = child_backend_provide_ldisc,
    .unthrottle = child_backend_unthrottle,
    .protocol = PROT_SSH,
};

static void child_channel_free(Channel *chan)
{
    AiChild *child = container_of(chan, AiChild, chan);
    child->ssh = NULL;
    if (!child->closing)
        win_ai_child_disconnected(child->window, child->stage);
}

static void child_open_confirmation(Channel *chan)
{
    AiChild *child = container_of(chan, AiChild, chan);
    WinGuiSeat *wgs = child->window;
    child->stage = CHILD_WAIT_PTY;
    sshfwd_request_pty(child->ssh, true, wgs->conf, wgs->term->cols, wgs->term->rows);
}

static void child_open_failed(Channel *chan, const char *error)
{
    /* The subsequent free callback reports the channel-open stage. */
}

static void child_close_after_failure(void *ctx)
{
    AiChild *child = ctx;
    if (child->ssh)
        sshfwd_initiate_close(child->ssh, NULL);
}

static void child_request_response(Channel *chan, bool success)
{
    AiChild *child = container_of(chan, AiChild, chan);
    if (child->closing || child->remote_close || child->eof || child->start_failed)
        return;
    if (!success) {
        child->start_failed = true;
        queue_toplevel_callback(child_close_after_failure, child);
        return;
    }
    if (child->stage == CHILD_WAIT_PTY) {
        child->stage = CHILD_WAIT_SHELL;
        child_backend_size(&child->backend, child->window->term->cols, child->window->term->rows);
        sshfwd_start_shell(child->ssh, true);
    } else if (child->stage == CHILD_WAIT_SHELL) {
        child->stage = CHILD_READY;
        win_ai_child_connected(child->window);
        if (child->ldisc)
            ldisc_check_sendok(child->ldisc);
    }
}

static size_t child_data(Channel *chan, bool is_stderr, const void *data, size_t len)
{
    AiChild *child = container_of(chan, AiChild, chan);
    return seat_output(&child->window->seat, is_stderr ? SEAT_OUTPUT_STDERR : SEAT_OUTPUT_STDOUT,
                       data, len);
}

static void child_eof(Channel *chan)
{
    AiChild *child = container_of(chan, AiChild, chan);
    child->eof = true;
    sshfwd_write_eof(child->ssh);
}

static void child_remote_close(Channel *chan)
{
    container_of(chan, AiChild, chan)->remote_close = true;
}

static void child_input_wanted(Channel *chan, bool wanted)
{
    AiChild *child = container_of(chan, AiChild, chan);
    child->input_wanted = wanted;
    if (wanted && child->ldisc)
        ldisc_check_sendok(child->ldisc);
}

static char *child_close_log(Channel *chan)
{
    return dupstr("PuTTY child terminal channel closed");
}

static bool child_exit_status(Channel *chan, int status)
{
    container_of(chan, AiChild, chan)->exit_code = status;
    return true;
}

static bool child_exit_signal(Channel *chan, ptrlen name, bool core, ptrlen msg)
{
    return child_exit_status(chan, INT_MAX);
}

static bool child_exit_signal_numeric(Channel *chan, int signal, bool core, ptrlen msg)
{
    return child_exit_status(chan, INT_MAX);
}

static const ChannelVtable child_channel_vt = {
    .free = child_channel_free,
    .open_confirmation = child_open_confirmation,
    .open_failed = child_open_failed,
    .send = child_data,
    .send_eof = child_eof,
    .remote_close = child_remote_close,
    .set_input_wanted = child_input_wanted,
    .log_close_msg = child_close_log,
    .want_close = chan_default_want_close,
    .rcvd_exit_status = child_exit_status,
    .rcvd_exit_signal = child_exit_signal,
    .rcvd_exit_signal_numeric = child_exit_signal_numeric,
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
    .request_response = child_request_response,
};

Backend *ai_child_backend_new(WinGuiSeat *window)
{
    AiChild *child = snew(AiChild);
    memset(child, 0, sizeof(*child));
    child->backend.vt = &child_backend_vt;
    child->chan.vt = &child_channel_vt;
    child->window = window;
    child->stage = CHILD_WAIT_OPEN;
    child->input_wanted = true;
    child->exit_code = -1;
    return &child->backend;
}

void ai_child_backend_open(Backend *backend, ConnectionLayer *connection)
{
    AiChild *child = container_of(backend, AiChild, backend);
    child->ssh = ssh_session_open(connection, &child->chan);
}
