#include <windows.h>
#include <stdio.h>
#include <string.h>
#include "command.h"
int ai_command_validate(const char *command,size_t length)
{
    wchar_t wide[AI_COMMAND_MAX+1]; int count,i,nonspace=0;
    if (!length || length>AI_COMMAND_MAX || memchr(command,0,length)) return 0;
    count=MultiByteToWideChar(CP_UTF8,MB_ERR_INVALID_CHARS,command,(int)length,wide,AI_COMMAND_MAX);
    if (!count) return 0;
    for(i=0;i<count;i++) {
        unsigned c=wide[i];
        if ((c<32 && c!='\n' && c!='\t') || (c>=127 && c<=159) ||
            c==0x061c || (c>=0x200b && c<=0x200f) || (c>=0x202a && c<=0x202e) ||
            (c>=0x2066 && c<=0x2069) || c==0xfeff) return 0;
        if (c!=' ' && c!='\n' && c!='\t') nonspace=1;
    }
    return nonspace;
}
int ai_command_wrap(const char *command,const char *token,char *out,size_t cap)
{
    char quoted[AI_COMMAND_MAX*4+3]; size_t n=0,i;
    int length;
    if (!ai_command_validate(command,strlen(command))) return 0;
    quoted[n++]='\'';
    for(i=0;command[i];i++) {
        if (command[i]=='\'') { memcpy(quoted+n,"'\\''",4); n+=4; }
        else quoted[n++]=command[i];
    }
    quoted[n++]='\''; quoted[n]=0;
    /* eval receives exactly the approved script as one quoted argument. No
     * markers are emitted before a complete shell line has been parsed.
     * exit/exec, a malicious shell or a long interactive command may prevent
     * END; the broker then reports unknown/timeout rather than inventing success. */
    length=snprintf(out,cap,"{ printf '\\036PAI:%s:BEGIN\\037\\n'; eval %s; __pai_rc=$?; printf '\\036PAI:%s:END:%%s\\037\\n' \"$__pai_rc\"; }\r",token,quoted,token);
    /* Keep a single physical line within common POSIX tty canonical limits. */
    return length>0 && length<4000 && (size_t)length<cap;
}
