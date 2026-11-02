#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include "command.h"
#define CHECK(x) do { if (!(x)) { fprintf(stderr,"FAILED line %d: %s\n",__LINE__,#x); return 1; } } while(0)
int main(int argc,char **argv)
{
    char input[AI_COMMAND_MAX+2],output[AI_INPUT_LINE_MAX]; size_t n;
    const char *token="0123456789abcdef0123456789abcdef";
    if(argc==2 && !strcmp(argv[1],"--wrap")) {
        n=fread(input,1,sizeof(input)-1,stdin); input[n]=0;
        if (!ai_command_validate(input,n) || !ai_command_wrap(input,token,output,sizeof(output))) return 2;
        fputs(output,stdout); return 0;
    }
    CHECK(ai_command_validate("ip -br addr",11));
    CHECK(ai_command_validate("printf '%s' 'a' | cat\nprintf done",31));
    CHECK(!ai_command_validate("ip\rreboot",9));
    CHECK(!ai_command_validate("a\0b",3));
    CHECK(!ai_command_validate("\033[31m",5));
    CHECK(!ai_command_validate("\xff",1));
    CHECK(!ai_command_validate("ip\xe2\x80\xae",5));
    CHECK(ai_command_wrap("printf '%s' \"hello\"",token,output,sizeof(output)));
    CHECK(strstr(output,"eval 'printf '\\''%s'\\'' \"hello\"'")!=NULL);
    memset(input,'\'',1500); input[1500]=0;
    CHECK(!ai_command_wrap(input,token,output,sizeof(output)));
    puts("{\"test\":\"native_command_validation_and_quoting\",\"passed\":true}");
    return 0;
}
