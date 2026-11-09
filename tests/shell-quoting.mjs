// Check the exact bytes the native bridge delivers to a terminal.
import {spawnSync} from 'node:child_process';
import {mkdirSync,mkdtempSync,readFileSync,rmdirSync,unlinkSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
const root=resolve(dirname(fileURLToPath(import.meta.url)),'..');
const emitter=resolve(process.env.PUTTY_TEST_COMMAND_NATIVE??resolve(root,'build-ai/Release/test_ai_command.exe'));
const bash=process.env.PUTTY_TEST_BASH || 'C:/Program Files/Git/bin/bash.exe';
const temp=mkdtempSync(join(tmpdir(),'putty-custom-shell-'));
const checks=[];
const raw=script=>spawnSync(emitter,['--raw'],{input:Buffer.from(script),windowsHide:true});
try {
  for(const [name,command,expected] of [
    ['device_show','display current-configuration','display current-configuration\r'],
    ['device_mode_lines','system-view\ninterface GigabitEthernet 1/0/1\n','system-view\rinterface GigabitEthernet 1/0/1\r'],
    ['utf8_and_spaces','  show 描述  ','  show 描述  \r'],
    ['pipe_and_quotes',`printf '%s' "a'b" | cat`,`printf '%s' "a'b" | cat\r`],
    ['maximum_command',"'".repeat(2048),"'".repeat(2048)+'\r'],
  ]) {
    const input=raw(command);
    assert.equal(input.status,0,input.stderr?.toString());
    assert.deepEqual(input.stdout,Buffer.from(expected),name);
    checks.push(name);
  }
  for(const [name,script,expected,exit] of [
    ['single_quote',`printf '%s\\n' "it's correct"`,"it's correct",0],
    ['pipe',`printf '%s' 'pipe-ok' | cat`,'pipe-ok',0],
    ['multiline_utf8',`printf '%s\\n' '部署测试'\nprintf '%s' 'second-line'`,'部署测试\nsecond-line',0],
    ['script_and_redirect',`printf '%s' 'deployment-ok' > output.txt\ncat output.txt`,'deployment-ok',0],
    ['nonzero',`false`,'',1],
  ]) {
    const input=raw(script);
    assert.equal(input.status,0,input.stderr?.toString());
    const run=spawnSync(bash,['--noprofile','--norc','-s'],{input:input.stdout.toString('utf8').replaceAll('\r','\n'),encoding:'utf8',cwd:temp,windowsHide:true});
    assert.equal(run.status,exit,run.stderr);
    assert(run.stdout.includes(expected),run.stdout); checks.push(name);
  }
  assert.equal(readFileSync(join(temp,'output.txt'),'utf8'),'deployment-ok');
  for(const script of ['ip\x1b[A','ip\rreboot','a\0b','x'.repeat(2049),' \n\t']) {
    assert.notEqual(raw(script).status,0);
  }
  checks.push('control_and_length_rejection');
  const report={passed:true,checks,scope:'Byte-exact native input and local Bash syntax check. No remote SSH or real network device.'};
  const reportDir=resolve(process.env.PUTTY_SMOKE_REPORT_DIR??resolve(root,'docs'));
  mkdirSync(reportDir,{recursive:true});
  writeFileSync(resolve(reportDir,'custom-shell-results.json'),JSON.stringify(report,null,2)); console.log(JSON.stringify(report,null,2));
} finally {
  try { unlinkSync(join(temp,'output.txt')); } catch(e) { if(e.code!=='ENOENT')throw e; }
  rmdirSync(temp);
}
