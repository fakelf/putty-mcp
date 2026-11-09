#include "putty.h"
#include "terminal.h"

void modalfatalbox(const char *p, ...)
{
    va_list ap;
    fprintf(stderr, "FATAL ERROR: ");
    va_start(ap, p);
    vfprintf(stderr, p, ap);
    va_end(ap);
    fputc('\n', stderr);
    exit(1);
}

const char *const appname = "test_terminal";

char *platform_default_s(const char *name)
{ return NULL; }
bool platform_default_b(const char *name, bool def)
{ return def; }
int platform_default_i(const char *name, int def)
{ return def; }
FontSpec *platform_default_fontspec(const char *name)
{ return fontspec_new_default(); }
Filename *platform_default_filename(const char *name)
{ return filename_from_str(""); }

const struct BackendVtable *const backends[] = { NULL };

typedef struct Mock {
    Terminal *term;
    Conf *conf;
    struct unicode_data ucsdata[1];
    strbuf *title;

    strbuf *context;

    bool any_test_failed;

    TermWin tw;
} Mock;

static bool mock_setup_draw_ctx(TermWin *win) { return false; }
static void mock_draw_text(TermWin *win, int x, int y, wchar_t *text, int len,
                           unsigned long attrs, int lattrs, truecolour tc) {}
static void mock_draw_cursor(TermWin *win, int x, int y, wchar_t *text,
                             int len, unsigned long attrs, int lattrs,
                             truecolour tc) {}
static void mock_set_raw_mouse_mode(TermWin *win, bool enable) {}
static void mock_set_raw_mouse_mode_pointer(TermWin *win, bool enable) {}
static void mock_palette_set(TermWin *win, unsigned start, unsigned ncolours,
                             const rgb *colours) {}
static void mock_palette_get_overrides(TermWin *tw, Terminal *term) {}
static void mock_set_title(TermWin *win, const char *title, int codepage);
static void mock_set_icon_title(TermWin *win, const char *title, int cp) {}

static const TermWinVtable mock_termwin_vt = {
    .setup_draw_ctx = mock_setup_draw_ctx,
    .draw_text = mock_draw_text,
    .draw_cursor = mock_draw_cursor,
    .set_title = mock_set_title,
    .set_icon_title = mock_set_icon_title,
    .set_raw_mouse_mode = mock_set_raw_mouse_mode,
    .set_raw_mouse_mode_pointer = mock_set_raw_mouse_mode_pointer,
    .palette_set = mock_palette_set,
    .palette_get_overrides = mock_palette_get_overrides,
};

static Mock *mock_new(void)
{
    Mock *mk = snew(Mock);
    memset(mk, 0, sizeof(*mk));

    mk->conf = conf_new();
    do_defaults(NULL, mk->conf);

    init_ucs_generic(mk->conf, mk->ucsdata);
    mk->ucsdata->line_codepage = CP_ISO8859_1;

    mk->context = strbuf_new();
    mk->title = strbuf_new();

    mk->tw.vt = &mock_termwin_vt;

    return mk;
}

static void mock_free(Mock *mk)
{
    strbuf_free(mk->context);
    conf_free(mk->conf);
    term_free(mk->term);
    strbuf_free(mk->title);
    sfree(mk);
}

static void mock_set_title(TermWin *win, const char *title, int codepage)
{
    Mock *mk = container_of(win, Mock, tw);
    strbuf_clear(mk->title);
    put_dataz(mk->title, title);
}

static void reset(Mock *mk)
{
    term_pwron(mk->term, true);
    term_size(mk->term, 24, 80, 0);
    term_set_trust_status(mk->term, false);
    strbuf_clear(mk->context);
    strbuf_clear(mk->title);
}

#if 0

static void test_context(Mock *mk, const char *fmt, ...)
{
    strbuf_clear(mk->context);
    va_list ap;
    va_start(ap, fmt);
    put_fmtv(mk->context, fmt, ap);
    va_end(ap);
}

#endif

static void report_fail(Mock *mk, const char *file, int line,
                        const char *fmt, ...)
{
    printf("%s:%d", file, line);
    if (mk->context->len)
        printf(" (%s)", mk->context->s);
    printf(": ");
    va_list ap;
    va_start(ap, fmt);
    vprintf(fmt, ap);
    va_end(ap);
    printf("\n");
    mk->any_test_failed = true;
}

static inline void check_iequal(Mock *mk, const char *file, int line,
                                long long lhs, long long rhs)
{
    if (lhs != rhs)
        report_fail(mk, file, line, "%lld != %lld / %#llx != %#llx",
                    lhs, rhs, lhs, rhs);
}

static inline void check_sequal(Mock *mk, const char *file, int line,
                                const char *lhs, const char *rhs)
{
    if (strcmp(lhs, rhs) != 0)
        report_fail(mk, file, line, "'%s' != '%s'", lhs, rhs);
}

#define IEQUAL(lhs, rhs) check_iequal(mk, __FILE__, __LINE__, lhs, rhs)
#define SEQUAL(lhs, rhs) check_sequal(mk, __FILE__, __LINE__, lhs, rhs)

static inline void term_datapl(Terminal *term, ptrlen pl)
{
    term_data(term, pl.ptr, pl.len);
}

static struct termchar get_termchar(Terminal *term, int x, int y)
{
    termline *tl = term_get_line(term, y);
    termchar tc;
    if (0 <= x && x < tl->cols)
        tc = tl->chars[x];
    else
        tc = term->erase_char;
    term_release_line(tl);
    return tc;
}

static unsigned short get_lineattr(Terminal *term, int y)
{
    termline *tl = term_get_line(term, y);
    unsigned short lattr = tl->lattr;
    term_release_line(tl);
    return lattr;
}

static void test_hello_world(Mock *mk)
{
    /* A trivial test just to kick off this test framework */
    mk->ucsdata->line_codepage = CP_ISO8859_1;

    reset(mk);
    term_datapl(mk->term, PTRLEN_LITERAL("hello, world"));
    IEQUAL(mk->term->curs.x, 12);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(get_termchar(mk->term, 0, 0).chr, CSET_ASCII | 'h');
    IEQUAL(get_termchar(mk->term, 1, 0).chr, CSET_ASCII | 'e');
    IEQUAL(get_termchar(mk->term, 2, 0).chr, CSET_ASCII | 'l');
    IEQUAL(get_termchar(mk->term, 3, 0).chr, CSET_ASCII | 'l');
    IEQUAL(get_termchar(mk->term, 4, 0).chr, CSET_ASCII | 'o');
    IEQUAL(get_termchar(mk->term, 5, 0).chr, CSET_ASCII | ',');
    IEQUAL(get_termchar(mk->term, 6, 0).chr, CSET_ASCII | ' ');
    IEQUAL(get_termchar(mk->term, 7, 0).chr, CSET_ASCII | 'w');
    IEQUAL(get_termchar(mk->term, 8, 0).chr, CSET_ASCII | 'o');
    IEQUAL(get_termchar(mk->term, 9, 0).chr, CSET_ASCII | 'r');
    IEQUAL(get_termchar(mk->term, 10, 0).chr, CSET_ASCII | 'l');
    IEQUAL(get_termchar(mk->term, 11, 0).chr, CSET_ASCII | 'd');
}

static void test_wrap(Mock *mk)
{
    /* Test behaviour when printing characters wrap to the next line */
    mk->ucsdata->line_codepage = CP_UTF8;

    /* Print 'abc' without enough space for the c, in wrapping mode */
    reset(mk);
    mk->term->curs.x = 78;
    mk->term->curs.y = 0;
    mk->term->wrap = true;
    /* The 'a' prints without anything unusual happening */
    term_datapl(mk->term, PTRLEN_LITERAL("a"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, CSET_ASCII | 'a');
    /* The 'b' prints, leaving the cursor where it is with wrapnext set */
    term_datapl(mk->term, PTRLEN_LITERAL("b"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 1);
    IEQUAL(get_lineattr(mk->term, 0), 0);
    IEQUAL(get_termchar(mk->term, 79, 0).chr, CSET_ASCII | 'b');
    /* And now the 'c' causes a deferred wrap and goes to the next line */
    term_datapl(mk->term, PTRLEN_LITERAL("c"));
    IEQUAL(mk->term->curs.x, 1);
    IEQUAL(mk->term->curs.y, 1);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_lineattr(mk->term, 0), LATTR_WRAPPED);
    IEQUAL(get_termchar(mk->term, 79, 0).chr, CSET_ASCII | 'b');
    IEQUAL(get_termchar(mk->term, 0, 1).chr, CSET_ASCII | 'c');
    /* If we backspace once, the cursor moves back on to the c */
    term_datapl(mk->term, PTRLEN_LITERAL("\b"));
    IEQUAL(mk->term->curs.x, 0);
    IEQUAL(mk->term->curs.y, 1);
    IEQUAL(mk->term->wrapnext, 0);
    /* Now backspace again, and the cursor returns to the b */
    term_datapl(mk->term, PTRLEN_LITERAL("\b"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);

    /* Now try it with a double-width character in place of ab */
    mk->term->curs.x = 78;
    mk->term->curs.y = 0;
    mk->term->wrap = true;
    /* The DW character goes directly to the wrapnext state */
    term_datapl(mk->term, PTRLEN_LITERAL("\xEA\xB0\x80"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 1);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, 0xAC00);
    IEQUAL(get_termchar(mk->term, 79, 0).chr, UCSWIDE);
    /* And the 'c' causes a deferred wrap as before */
    term_datapl(mk->term, PTRLEN_LITERAL("c"));
    IEQUAL(mk->term->curs.x, 1);
    IEQUAL(mk->term->curs.y, 1);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_lineattr(mk->term, 0), LATTR_WRAPPED);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, 0xAC00);
    IEQUAL(get_termchar(mk->term, 79, 0).chr, UCSWIDE);
    IEQUAL(get_termchar(mk->term, 0, 1).chr, CSET_ASCII | 'c');
    /* If we backspace once, the cursor moves back on to the c */
    term_datapl(mk->term, PTRLEN_LITERAL("\b"));
    IEQUAL(mk->term->curs.x, 0);
    IEQUAL(mk->term->curs.y, 1);
    IEQUAL(mk->term->wrapnext, 0);
    /* Now backspace again, and the cursor goes to the RHS of the DW char */
    term_datapl(mk->term, PTRLEN_LITERAL("\b"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);

    /* Now put the DW character in place of bc */
    reset(mk);
    mk->term->curs.x = 78;
    mk->term->curs.y = 0;
    mk->term->wrap = true;
    /* The 'a' prints as before */
    term_datapl(mk->term, PTRLEN_LITERAL("a"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, CSET_ASCII | 'a');
    /* The DW character wraps, setting LATTR_WRAPPED2 */
    term_datapl(mk->term, PTRLEN_LITERAL("\xEA\xB0\x80"));
    IEQUAL(mk->term->curs.x, 2);
    IEQUAL(mk->term->curs.y, 1);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_lineattr(mk->term, 0), LATTR_WRAPPED | LATTR_WRAPPED2);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, CSET_ASCII | 'a');
    IEQUAL(get_termchar(mk->term, 79, 0).chr, CSET_ASCII | ' ');
    IEQUAL(get_termchar(mk->term, 0, 1).chr, 0xAC00);
    IEQUAL(get_termchar(mk->term, 1, 1).chr, UCSWIDE);
    /* If we backspace once, cursor moves to the RHS of the DW char */
    term_datapl(mk->term, PTRLEN_LITERAL("\b"));
    IEQUAL(mk->term->curs.x, 1);
    IEQUAL(mk->term->curs.y, 1);
    IEQUAL(mk->term->wrapnext, 0);
    /* Backspace again, and cursor moves from RHS to LHS of that char */
    term_datapl(mk->term, PTRLEN_LITERAL("\b"));
    IEQUAL(mk->term->curs.x, 0);
    IEQUAL(mk->term->curs.y, 1);
    IEQUAL(mk->term->wrapnext, 0);
    /* Now backspace again, and the cursor skips the empty column so
     * that it can return to the previous logical character, to wit, the a */
    term_datapl(mk->term, PTRLEN_LITERAL("\b"));
    IEQUAL(mk->term->curs.x, 78);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);

    /* Print 'ab' up to the rightmost column, and then backspace */
    reset(mk);
    mk->term->curs.x = 78;
    mk->term->curs.y = 0;
    mk->term->wrap = true;
    /* As before, the 'ab' put us in the rightmost column with wrapnext set */
    term_datapl(mk->term, PTRLEN_LITERAL("ab"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 1);
    IEQUAL(get_lineattr(mk->term, 0), 0);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, CSET_ASCII | 'a');
    IEQUAL(get_termchar(mk->term, 79, 0).chr, CSET_ASCII | 'b');
    /* Backspacing just clears the wrapnext flag, so we're logically
     * back on the b again */
    term_datapl(mk->term, PTRLEN_LITERAL("\b"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);

    /* For completeness, the easy case: just print 'a' then backspace */
    reset(mk);
    mk->term->curs.x = 78;
    mk->term->curs.y = 0;
    mk->term->wrap = true;
    /* 'a' printed in column n-1 takes us to column n */
    term_datapl(mk->term, PTRLEN_LITERAL("a"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_lineattr(mk->term, 0), 0);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, CSET_ASCII | 'a');
    /* Backspacing moves us back a space on to the a */
    term_datapl(mk->term, PTRLEN_LITERAL("\b"));
    IEQUAL(mk->term->curs.x, 78);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);

    /*
     * Now test the special cases that arise when the terminal is only
     * one column wide!
     */

    reset(mk);
    term_size(mk->term, 24, 1, 0);
    mk->term->curs.x = 0;
    mk->term->curs.y = 0;
    mk->term->wrap = true;
    /* Printing a single-width character takes us into wrapnext immediately */
    term_datapl(mk->term, PTRLEN_LITERAL("a"));
    IEQUAL(mk->term->curs.x, 0);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 1);
    IEQUAL(get_lineattr(mk->term, 0), 0);
    IEQUAL(get_termchar(mk->term, 0, 0).chr, CSET_ASCII | 'a');
    /* Printing a second one wraps, and takes us _back_ to wrapnext */
    term_datapl(mk->term, PTRLEN_LITERAL("b"));
    IEQUAL(mk->term->curs.x, 0);
    IEQUAL(mk->term->curs.y, 1);
    IEQUAL(mk->term->wrapnext, 1);
    IEQUAL(get_lineattr(mk->term, 0), LATTR_WRAPPED);
    IEQUAL(get_termchar(mk->term, 0, 0).chr, CSET_ASCII | 'a');
    IEQUAL(get_termchar(mk->term, 0, 1).chr, CSET_ASCII | 'b');
    /* Backspacing once clears the wrapnext flag, putting us on the b */
    term_datapl(mk->term, PTRLEN_LITERAL("\b"));
    IEQUAL(mk->term->curs.x, 0);
    IEQUAL(mk->term->curs.y, 1);
    IEQUAL(mk->term->wrapnext, 0);
    /* Backspacing again returns to the previous line, putting us on the a */
    term_datapl(mk->term, PTRLEN_LITERAL("\b"));
    IEQUAL(mk->term->curs.x, 0);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);

    /* And now try with a double-width character */
    reset(mk);
    term_size(mk->term, 24, 1, 0);
    mk->term->curs.x = 0;
    mk->term->curs.y = 0;
    mk->term->wrap = true;
    /* DW character won't fit at all, so it transforms into U+FFFD
     * REPLACEMENT CHARACTER and then behaves like a SW char */
    term_datapl(mk->term, PTRLEN_LITERAL("\xEA\xB0\x80"));
    IEQUAL(mk->term->curs.x, 0);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 1);
    IEQUAL(get_lineattr(mk->term, 0), 0);
    IEQUAL(get_termchar(mk->term, 0, 0).chr, 0xFFFD);
}

static void test_nonwrap(Mock *mk)
{
    /* Test behaviour when printing characters hit end of line without wrap.
     * The wrapnext flag is never set in this mode. */
    mk->ucsdata->line_codepage = CP_UTF8;

    /* Print 'abc' without enough space for the c */
    reset(mk);
    mk->term->curs.x = 78;
    mk->term->curs.y = 0;
    mk->term->wrap = false;
    /* The 'a' prints without anything unusual happening */
    term_datapl(mk->term, PTRLEN_LITERAL("a"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, CSET_ASCII | 'a');
    /* The 'b' prints, leaving the cursor where it is */
    term_datapl(mk->term, PTRLEN_LITERAL("b"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_lineattr(mk->term, 0), 0);
    IEQUAL(get_termchar(mk->term, 79, 0).chr, CSET_ASCII | 'b');
    /* The 'c' overwrites the b */
    term_datapl(mk->term, PTRLEN_LITERAL("c"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_lineattr(mk->term, 0), 0);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, CSET_ASCII | 'a');
    IEQUAL(get_termchar(mk->term, 79, 0).chr, CSET_ASCII | 'c');
    /* Since wrapnext was never set, backspacing returns us to the a */
    term_datapl(mk->term, PTRLEN_LITERAL("\b"));
    IEQUAL(mk->term->curs.x, 78);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);

    /* Now try it with a double-width character in place of ab */
    mk->term->curs.x = 78;
    mk->term->curs.y = 0;
    mk->term->wrap = false;
    /* The DW character occupies the rightmost two columns */
    term_datapl(mk->term, PTRLEN_LITERAL("\xEA\xB0\x80"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, 0xAC00);
    IEQUAL(get_termchar(mk->term, 79, 0).chr, UCSWIDE);
    /* The 'c' must overprint the RHS of the DW char, clearing the LHS */
    term_datapl(mk->term, PTRLEN_LITERAL("c"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_lineattr(mk->term, 0), 0);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, CSET_ASCII | ' ');
    IEQUAL(get_termchar(mk->term, 79, 0).chr, CSET_ASCII | 'c');

    /* Now put the DW char in place of the bc */
    reset(mk);
    mk->term->curs.x = 78;
    mk->term->curs.y = 0;
    mk->term->wrap = false;
    /* The 'a' prints as before */
    term_datapl(mk->term, PTRLEN_LITERAL("a"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, CSET_ASCII | 'a');
    /* The DW char won't fit, so turns into U+FFFD REPLACEMENT CHARACTER */
    term_datapl(mk->term, PTRLEN_LITERAL("\xEA\xB0\x80"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_lineattr(mk->term, 0), 0);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, CSET_ASCII | 'a');
    IEQUAL(get_termchar(mk->term, 79, 0).chr, 0xFFFD);

    /* Just for completeness, try both of those together */
    reset(mk);
    mk->term->curs.x = 78;
    mk->term->curs.y = 0;
    mk->term->wrap = false;
    /* First DW character occupies the rightmost columns */
    term_datapl(mk->term, PTRLEN_LITERAL("\xEA\xB0\x80"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, 0xAC00);
    IEQUAL(get_termchar(mk->term, 79, 0).chr, UCSWIDE);
    /* Second DW char becomes U+FFFD, overwriting RHS of the first one */
    term_datapl(mk->term, PTRLEN_LITERAL("\xEA\xB0\x81"));
    IEQUAL(mk->term->curs.x, 79);
    IEQUAL(mk->term->curs.y, 0);
    IEQUAL(mk->term->wrapnext, 0);
    IEQUAL(get_lineattr(mk->term, 0), 0);
    IEQUAL(get_termchar(mk->term, 78, 0).chr, CSET_ASCII | ' ');
    IEQUAL(get_termchar(mk->term, 79, 0).chr, 0xFFFD);
}

static void test_wintitle(Mock *mk)
{
    reset(mk);
    term_datapl(mk->term, PTRLEN_LITERAL("\033]0;foo\033\\"));
    term_update(mk->term);
    SEQUAL(mk->title->s, "foo");
    term_datapl(mk->term, PTRLEN_LITERAL("\033]0;bar\007"));
    term_update(mk->term);
    SEQUAL(mk->title->s, "bar");

    /* Regression test for a bug in which a DCS string was
     * accidentally treated as a title-setting OSC 0. We expect that
     * this is not a window-title setting sequence, and leaves the
     * title unchanged. */
    term_datapl(mk->term, PTRLEN_LITERAL("\033Pzz\033\\"));
    term_update(mk->term);
    SEQUAL(mk->title->s, "bar");
}

static void test_visible_text(Mock *mk)
{
    reset(mk);
    term_size(mk->term, 2, 20, 10);
    term_datapl(mk->term, PTRLEN_LITERAL("old\r\nrow one\r\nrow two"));
    size_t length;
    const wchar_t *visible = term_copy_visible(mk->term, &length);
    IEQUAL(length > 7 && wcsncmp(visible, L"row one", 7) == 0, 1);

    term_datapl(mk->term, PTRLEN_LITERAL("\033[?1049h\033[Htop old\r\nPID"));
    term_datapl(mk->term, PTRLEN_LITERAL("\033[H\033[Ktop new"));
    visible = term_copy_visible(mk->term, &length);
    IEQUAL(length > 7 && wcsncmp(visible, L"top new", 7) == 0, 1);
}

static void test_prompt_snapshot(Mock *mk)
{
    char prompt[256];
    mk->ucsdata->line_codepage = CP_UTF8;
    reset(mk);
    term_datapl(mk->term, PTRLEN_LITERAL("clipboard contents"));
    size_t clipboard_length;
    const wchar_t *clipboard = term_copy_visible(mk->term, &clipboard_length);
    term_datapl(mk->term, PTRLEN_LITERAL("\r\033[2K\033[32m  user@host:~$\033[0m  "));
    size_t length = term_snapshot_prompt(mk->term, prompt, sizeof(prompt));
    IEQUAL(length, sizeof("user@host:~$") - 1);
    IEQUAL(memcmp(prompt, "user@host:~$", sizeof("user@host:~$") - 1), 0);
    IEQUAL(mk->term->last_selected_text == clipboard, 1);
    IEQUAL(mk->term->last_selected_len - 1, clipboard_length);
    IEQUAL(mk->term->last_selected_text &&
           !wcsncmp(mk->term->last_selected_text, L"clipboard contents", 18), 1);
    IEQUAL(term_snapshot_prompt(mk->term, prompt, 5), 0);

    reset(mk);
    const char unicode[] = "e\xcc\x81\xe7\x94\xa8\xf0\x9f\x9a\x80@host:~%";
    term_data(mk->term, unicode, sizeof(unicode) - 1);
    length = term_snapshot_prompt(mk->term, prompt, sizeof(unicode) - 1);
    IEQUAL(length, sizeof(unicode) - 1);
    IEQUAL(memcmp(prompt, unicode, sizeof(unicode) - 1), 0);

    static const char *const network_prompts[] = {
        "<Switch>", "[Switch]", "[Switch-if]", "[*Switch]", "[~Switch]",
        "[Switch-GigabitEthernet1/0/1]", "[~Switch-100GE1/0/4.12]",
        "[Switch-Vlan-interface10]",
    };
    for (size_t i = 0; i < lenof(network_prompts); ++i) {
        reset(mk);
        size_t expected = strlen(network_prompts[i]);
        term_data(mk->term, network_prompts[i], expected);
        IEQUAL(term_snapshot_prompt(mk->term, prompt, sizeof(prompt)), expected);
        IEQUAL(memcmp(prompt, network_prompts[i], expected), 0);
    }

    static const char *const ambiguous[] = {
        "login:", "Password >", "Enter MFA code #", ">", "heredoc>",
        "router# echo #", "router# cmd\033[3D", "router#\033[?25l",
        "\033[?1049hrouter#", "router#\033[",
        "[Y/N]", "[Y/N/C]", "[yes/no]", "[confirm]", "[OK]",
        "Continue? [Y/N]", "[flash:/startup.cfg]", "[123]", "[*]",
        "[Switch] display [other]", "[Switch-GigabitEthernet1//0/1]",
    };
    for (size_t i = 0; i < lenof(ambiguous); ++i) {
        reset(mk);
        term_data(mk->term, ambiguous[i], strlen(ambiguous[i]));
        IEQUAL(term_snapshot_prompt(mk->term, prompt, sizeof(prompt)), 0);
    }

    reset(mk);
    term_size(mk->term, 24, 12, 0);
    term_datapl(mk->term, PTRLEN_LITERAL("user@host:/very/long/path$ "));
    IEQUAL(term_snapshot_prompt(mk->term, prompt, sizeof(prompt)), 0);

    reset(mk);
    term_size(mk->term, 2, 80, 10);
    term_datapl(mk->term, PTRLEN_LITERAL("old\r\noutput\r\nrouter> "));
    IEQUAL(term_snapshot_prompt(mk->term, prompt, sizeof(prompt)), 7);
    term_scroll(mk->term, 0, -1);
    IEQUAL(term_snapshot_prompt(mk->term, prompt, sizeof(prompt)), 0);
}

static void check_mcp_key(Mock *mk, const char *name, ptrlen expected)
{
    char output[TERM_KEY_ENCODE_MAX];
    size_t length = term_encode_mcp_key(mk->term, ptrlen_from_asciz(name), output);
    IEQUAL(length, expected.len);
    if (length == expected.len && length)
        IEQUAL(memcmp(output, expected.ptr, length), 0);
}

static void test_mcp_keys(Mock *mk)
{
    reset(mk);
    Terminal *term = mk->term;
    term->funky_type = FUNKY_XTERM_216;
    term->sharrow_type = SHARROW_BITMAP;
    term->app_cursor_keys = term->no_applic_c = false;
    check_mcp_key(mk, "space", PTRLEN_LITERAL(" "));
    check_mcp_key(mk, "q", PTRLEN_LITERAL("q"));
    check_mcp_key(mk, "ctrl+space", PTRLEN_LITERAL("\0"));
    check_mcp_key(mk, "ctrl+shift+c", PTRLEN_LITERAL("\003"));
    check_mcp_key(mk, "ctrl+8", PTRLEN_LITERAL("\177"));
    check_mcp_key(mk, "alt++", PTRLEN_LITERAL("\033+"));
    check_mcp_key(mk, "shift+a", PTRLEN_LITERAL("A"));
    check_mcp_key(mk, "shift+tab", PTRLEN_LITERAL("\033[Z"));
    check_mcp_key(mk, "up", PTRLEN_LITERAL("\033[A"));
    check_mcp_key(mk, "ctrl+alt+shift+left", PTRLEN_LITERAL("\033[1;8D"));
    check_mcp_key(mk, "alt+f4", PTRLEN_LITERAL("\033[1;3S"));
    check_mcp_key(mk, "ctrl+page_down", PTRLEN_LITERAL("\033[6;5~"));
    check_mcp_key(mk, "f20", PTRLEN_LITERAL("\033[34~"));
    check_mcp_key(mk, "f21", PTRLEN_LITERAL(""));
    check_mcp_key(mk, "ctrl+tab", PTRLEN_LITERAL(""));
    check_mcp_key(mk, "alt+ctrl+c", PTRLEN_LITERAL(""));
    term->app_cursor_keys = true;
    check_mcp_key(mk, "up", PTRLEN_LITERAL("\033OA"));
    term->app_keypad_keys = true;
    term->no_applic_k = false;
    check_mcp_key(mk, "keypad_5", PTRLEN_LITERAL("\033Ou"));
    check_mcp_key(mk, "keypad_enter", PTRLEN_LITERAL("\033OM"));
    term->app_keypad_keys = false;
    check_mcp_key(mk, "keypad_5", PTRLEN_LITERAL("5"));
    term->bksp_is_delete = true;
    check_mcp_key(mk, "backspace", PTRLEN_LITERAL("\177"));
    check_mcp_key(mk, "shift+backspace", PTRLEN_LITERAL("\b"));
    term->cr_lf_return = true;
    check_mcp_key(mk, "enter", PTRLEN_LITERAL("\r\n"));
    check_mcp_key(mk, "alt+enter", PTRLEN_LITERAL("\033\r"));
    term->funky_type = FUNKY_SCO;
    check_mcp_key(mk, "f13", PTRLEN_LITERAL(""));
}

static void test_mcp_screen(Mock *mk)
{
    mk->ucsdata->line_codepage = CP_UTF8;
    reset(mk);
    term_size(mk->term, 3, 30, 10);
    term_datapl(mk->term, PTRLEN_LITERAL(
        "old\r\nfirst\r\n \033[31m\xe9\x9b\xaa\xf0\x9f\x98\x80\033[0m\r\nlast"));
    size_t clipboard_length;
    const wchar_t *clipboard = term_copy_visible(mk->term, &clipboard_length);
    term_scroll(mk->term, 0, -1);

    char output[128];
    bool truncated;
    const char expected[] = "first\n \xe9\x9b\xaa\xf0\x9f\x98\x80\nlast";
    IEQUAL(term_snapshot_screen(mk->term, output, sizeof(output), &truncated),
           sizeof(expected) - 1);
    IEQUAL(memcmp(output, expected, sizeof(expected) - 1), 0);
    IEQUAL(truncated, 0);
    IEQUAL(mk->term->last_selected_text == clipboard, 1);
    IEQUAL(mk->term->last_selected_len - 1, clipboard_length);

    const char prefix[] = "first\n \xe9\x9b\xaa";
    IEQUAL(term_snapshot_screen(mk->term, output, sizeof(prefix), &truncated),
           sizeof(prefix) - 1);
    IEQUAL(memcmp(output, prefix, sizeof(prefix) - 1), 0);
    IEQUAL(truncated, 1);

    term_datapl(mk->term, PTRLEN_LITERAL("\033[?1049h\033[Htop"));
    IEQUAL(term_snapshot_screen(mk->term, output, sizeof(output), &truncated), 5);
    IEQUAL(memcmp(output, "top\n\n", 5), 0);
}

static void test_mcp_text_encoding(Mock *mk)
{
    reset(mk);
    strbuf *encoded = strbuf_new();
    ptrlen text = PTRLEN_LITERAL("e\xcc\x81\xe9\x9b\xaa\n");
    mk->ucsdata->line_codepage = CP_UTF8;
    IEQUAL(term_encode_mcp_text(mk->term, text, encoded), 1);
    IEQUAL(encoded->len, text.len);
    IEQUAL(memcmp(encoded->s, text.ptr, text.len), 0);

    mk->ucsdata->line_codepage = CP_ISO8859_1;
    strbuf_clear(encoded);
    IEQUAL(term_encode_mcp_text(mk->term, PTRLEN_LITERAL("\xc3\xa9"), encoded), 1);
    IEQUAL(encoded->len, 1);
    IEQUAL((unsigned char)encoded->s[0], 0xe9);
    strbuf_clear(encoded);
    IEQUAL(term_encode_mcp_text(mk->term, PTRLEN_LITERAL("\xe9\x9b\xaa"), encoded), 0);
    IEQUAL(encoded->len, 0);
#ifdef _WIN32
    mk->ucsdata->line_codepage = 932; /* Windows Shift JIS */
    IEQUAL(term_encode_mcp_text(mk->term, PTRLEN_LITERAL("\xe3\x81\x82"), encoded), 1);
    IEQUAL(encoded->len, 2);
    IEQUAL(memcmp(encoded->s, "\x82\xa0", 2), 0);
    strbuf_clear(encoded);
    IEQUAL(term_encode_mcp_text(mk->term, PTRLEN_LITERAL("\xf0\x9f\x98\x80"), encoded), 0);
    IEQUAL(encoded->len, 0);
#endif
    strbuf_free(encoded);
}

typedef struct MockInputBackend {
    Backend backend;
    strbuf *sent;
    bool ready;
} MockInputBackend;

static void mock_input_send(Backend *backend, const char *data, size_t length)
{
    MockInputBackend *input = container_of(backend, MockInputBackend, backend);
    put_data(input->sent, data, length);
}

static bool mock_input_sendok(Backend *backend)
{
    return container_of(backend, MockInputBackend, backend)->ready;
}

static bool mock_input_option(Backend *backend, int option) { return false; }
static void mock_input_ldisc(Backend *backend, Ldisc *ldisc) {}

static const BackendVtable mock_input_vt = {
    .send = mock_input_send,
    .sendok = mock_input_sendok,
    .ldisc_option_state = mock_input_option,
    .provide_ldisc = mock_input_ldisc,
};

static void test_mcp_ldisc(Mock *mk)
{
    reset(mk);
    MockInputBackend input = { .backend = { .vt = &mock_input_vt },
                               .sent = strbuf_new(), .ready = true };
    Ldisc *ldisc = ldisc_create(mk->conf, mk->term, &input.backend, NULL);
    IEQUAL(ldisc_mcp_command_ready(ldisc), 1);
    char output[TERM_KEY_ENCODE_MAX];
    size_t length = term_encode_mcp_key(mk->term, PTRLEN_LITERAL("ctrl+space"), output);
    ldisc_send(ldisc, output, (int)length, true);
    IEQUAL(input.sent->len, 1);
    IEQUAL(input.sent->s[0], 0);
    input.ready = false;
    ldisc_send(ldisc, "q", 1, true);
    IEQUAL(ldisc_mcp_command_ready(ldisc), 0);
    IEQUAL(input.sent->len, 1);
    ldisc_free(ldisc);
    strbuf_free(input.sent);
}

static void test_mcp_credential_prompt(Mock *mk)
{
    static const char *const prompts[] = {
        "Password: ", "[sudo] password for local: ", "user@example's password: ",
        "Enter passphrase for key '/key': ", "Verification code: ", "OTP: ",
    };
    for (size_t i = 0; i < lenof(prompts); ++i) {
        reset(mk);
        term_data(mk->term, prompts[i], strlen(prompts[i]));
        IEQUAL(term_mcp_credential_prompt(mk->term), 1);
    }
    reset(mk);
    term_datapl(mk->term, PTRLEN_LITERAL("host:~/passwords$ "));
    IEQUAL(term_mcp_credential_prompt(mk->term), 0);
    reset(mk);
    term_datapl(mk->term, PTRLEN_LITERAL("\033[?1049hPassword: "));
    IEQUAL(term_mcp_credential_prompt(mk->term), 0);
}

int main(void)
{
    Mock *mk = mock_new();
    mk->term = term_init(mk->conf, mk->ucsdata, &mk->tw);

    test_hello_world(mk);
    test_wrap(mk);
    test_nonwrap(mk);
    test_wintitle(mk);
    test_visible_text(mk);
    test_prompt_snapshot(mk);
    test_mcp_keys(mk);
    test_mcp_screen(mk);
    test_mcp_text_encoding(mk);
    test_mcp_ldisc(mk);
    test_mcp_credential_prompt(mk);

    bool failed = mk->any_test_failed;
    mock_free(mk);

    if (failed) {
        printf("Test suite FAILED!\n");
        return 1;
    } else {
        printf("Test suite passed\n");
        return 0;
    }
}
