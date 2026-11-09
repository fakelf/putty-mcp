#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <fcntl.h>
#include <io.h>
#include "command.h"
#define CHECK(x) do { if (!(x)) { fprintf(stderr,"FAILED line %d: %s\n",__LINE__,#x); return 1; } } while(0)
int main(int argc,char **argv)
{
    char input[AI_COMMAND_MAX+2],output[AI_VI_FILE_COMMAND_MAX]; size_t n;
    if(argc==2 && !strcmp(argv[1],"--raw")) {
        if (_setmode(_fileno(stdin),_O_BINARY)<0 || _setmode(_fileno(stdout),_O_BINARY)<0) return 2;
        n=fread(input,1,sizeof(input),stdin);
        size_t size=ai_command_input(input,n,output,sizeof(output));
        return size && fwrite(output,1,size,stdout)==size?0:2;
    }
    CHECK(ai_command_validate("ip -br addr",11));
    CHECK(ai_command_validate("printf '%s' 'a' | cat\nprintf done",31));
    CHECK(!ai_command_validate("ip\rreboot",9));
    CHECK(!ai_command_validate("a\0b",3));
    CHECK(!ai_command_validate("\033[31m",5));
    CHECK(!ai_command_validate("\xff",1));
    CHECK(!ai_command_validate("ip\xe2\x80\xae",5));
    const char vi_text[]="draft :qa!\n\t\xe9\x9b\xaa";
    CHECK(ai_text_validate(vi_text,sizeof(vi_text)-1));
    CHECK(ai_text_validate(" \n\t",3));
    CHECK(!ai_text_validate("",0));
    CHECK(!ai_text_validate("a\0b",3));
    CHECK(!ai_text_validate("a\rb",3));
    CHECK(!ai_text_validate("\033[201~",6));
    CHECK(!ai_text_validate("\xc2\x9b",2));
    CHECK(!ai_text_validate("\xed\xa0\x80",3));
    CHECK(!ai_text_validate("\xe2\x80\xa8",3));
    CHECK(!ai_text_validate("\xe2\x80\xae",3));
    const char file_path[]="/home/fixture/test '\xc3\xa9'.sh";
    const char file_prefix[]="p='/home/fixture/test '\\''\xc3\xa9'\\''.sh'; test -f \"$p\" && test ! -L \"$p\" && ";
    CHECK(ai_file_path_validate(file_path,sizeof(file_path)-1));
    CHECK(ai_task_file_command(file_path,sizeof(file_path)-1,output,sizeof(output)));
    CHECK(!strncmp(output,file_prefix,sizeof(file_prefix)-1));
    CHECK(strstr(output,"-Z -m --cmd ") && strstr(output," -- \"$p\""));
    CHECK(!ai_task_file_command(file_path,sizeof(file_path)-1,output,20));
    CHECK(!ai_file_path_validate("relative",8));
    CHECK(!ai_file_path_validate("/file/../other",14));
    CHECK(!ai_file_path_validate("/file//other",12));
    CHECK(!ai_file_path_validate("/file/",6));
    CHECK(!ai_file_path_validate("/file\nname",10));
    CHECK(!ai_file_path_validate("/file\0name",10));
    memset(input,'\'',AI_VI_PATH_MAX+1);
    input[0]='/';
    CHECK(ai_task_file_command(input,AI_VI_PATH_MAX,output,AI_VI_FILE_COMMAND_MAX));
    CHECK(!ai_file_path_validate(input,AI_VI_PATH_MAX+1));
    const char cli[]="  display current-configuration  ";
    n=ai_command_input(cli,sizeof(cli)-1,output,sizeof(output));
    CHECK(n==sizeof(cli) && !memcmp(output,cli,sizeof(cli)-1) && output[n-1]=='\r');
    CHECK(ai_command_input("a\nb\n",4,output,sizeof(output))==4 && !memcmp(output,"a\rb\r",4));
    CHECK(!ai_command_input("abc",3,output,3));
    memset(input,'\'',AI_COMMAND_MAX);
    CHECK(ai_command_input(input,AI_COMMAND_MAX,output,sizeof(output))==AI_INPUT_LINE_MAX);
    CHECK(!memcmp(output,input,AI_COMMAND_MAX) && output[AI_COMMAND_MAX]=='\r');
    puts("{\"test\":\"native_command_validation_and_literal_input\",\"passed\":true}");
    return 0;
}
