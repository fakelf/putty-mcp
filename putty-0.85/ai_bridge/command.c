#include <windows.h>
#include <stdio.h>
#include <string.h>
#include "command.h"
static int validate_text(const char *command,size_t length,int editor)
{
    wchar_t wide[AI_COMMAND_MAX+1]; int count,i,nonspace=0;
    if (!length || length>AI_COMMAND_MAX || memchr(command,0,length)) return 0;
    count=MultiByteToWideChar(CP_UTF8,MB_ERR_INVALID_CHARS,command,(int)length,wide,AI_COMMAND_MAX);
    if (!count) return 0;
    for(i=0;i<count;i++) {
        unsigned c=wide[i];
        if ((c<32 && c!='\n' && c!='\t') || (c>=127 && c<=159) ||
            c==0x061c || (c>=0x200b && c<=0x200f) || (c>=0x202a && c<=0x202e) ||
            (c>=0x2066 && c<=0x2069) || c==0xfeff ||
            (editor && (c==0x2028 || c==0x2029))) return 0;
        if (c!=' ' && c!='\n' && c!='\t') nonspace=1;
    }
    return editor || nonspace;
}
int ai_command_validate(const char *command,size_t length)
{
    return validate_text(command,length,0);
}
int ai_text_validate(const char *text,size_t length)
{
    return validate_text(text,length,1);
}
int ai_file_path_validate(const char *path,size_t length)
{
    size_t start=1;
    if (length<2 || length>AI_VI_PATH_MAX || path[0]!='/' ||
        memchr(path,'\n',length) || memchr(path,'\t',length) ||
        !ai_text_validate(path,length)) return 0;
    for (size_t i=1;i<=length;i++) {
        if (i==length || path[i]=='/') {
            size_t count=i-start;
            if (!count || (count==1 && path[start]=='.') ||
                (count==2 && path[start]=='.' && path[start+1]=='.')) return 0;
            start=i+1;
        }
    }
    return 1;
}

/* The caller sizes quoted for four bytes per input byte, two quotes and NUL. */
static void shell_quote(const char *input,size_t length,char *quoted)
{
    size_t n=0;
    quoted[n++]='\'';
    for (size_t i=0;i<length;i++) {
        if (input[i]=='\'') { memcpy(quoted+n,"'\\''",4); n+=4; }
        else quoted[n++]=input[i];
    }
    quoted[n++]='\''; quoted[n]=0;
}

int ai_task_file_command(const char *path,size_t length,char *out,size_t cap)
{
    char quoted[AI_VI_PATH_MAX*4+3];
    if (!ai_file_path_validate(path,length)) return 0;
    shell_quote(path,length,quoted);
    int count=snprintf(out,cap,"p=%s; test -f \"$p\" && test ! -L \"$p\" && "
        "exec vi -Nu NONE -i NONE -n -Z -m --cmd "
        "'set encoding=utf-8 backspace=indent,eol,start nomodeline noautowrite noautowriteall' "
        "-c 'let g:putty_mcp_buf = bufnr(\"%%\") | let g:putty_mcp_path = expand(\"%%:p\") | "
        "let g:putty_mcp_resolved = resolve(g:putty_mcp_path) | "
        "let g:putty_mcp_mtime = getftime(g:putty_mcp_path) | "
        "let g:putty_mcp_size = getfsize(g:putty_mcp_path)' -- \"$p\"",quoted);
    return count>0 && (size_t)count<cap;
}

size_t ai_command_input(const char *command,size_t length,char *out,size_t cap)
{
    if (!ai_command_validate(command,length)) return 0;
    int final_enter=command[length-1]!='\n';
    if (length+final_enter>cap) return 0;
    for (size_t i=0;i<length;i++) out[i]=command[i]=='\n'?'\r':command[i];
    if (final_enter) out[length++]='\r';
    return length;
}
