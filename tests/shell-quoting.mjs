// Uses local Git Bash solely to verify POSIX parsing of the exact native wrapper.
// No SSH/network is used. Scripts write only inside an automatically deleted temp directory.
import {spawnSync} from 'node:child_process';
import {mkdtempSync,readFileSync,rmSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
const root=resolve(dirname(fileURLToPath(import.meta.url)),'..');
const emitter=resolve(root,'build-ai/Release/test_ai_command.exe');
const bash=process.env.PUTTY_TEST_BASH || 'C:/Program Files/Git/bin/bash.exe';
const temp=mkdtempSync(join(tmpdir(),'putty-custom-shell-'));
const checks=[];
try {
  for(const [name,script,expected,exit] of [
    ['single_quote',`printf '%s\\n' "it's correct"`,"it's correct",0],
    ['pipe',`printf '%s' 'pipe-ok' | cat`,'pipe-ok',0],
    ['multiline_utf8',`printf '%s\\n' '部署测试'\nprintf '%s' 'second-line'`,'部署测试\nsecond-line',0],
    ['script_and_redirect',`printf '%s' 'deployment-ok' > output.txt\ncat output.txt`,'deployment-ok',0],
    ['nonzero',`false`,'',1],
  ]) {
    const wrapped=spawnSync(emitter,['--wrap'],{input:script,encoding:'utf8',windowsHide:true});
    assert.equal(wrapped.status,0,wrapped.stderr);
    const run=spawnSync(bash,['--noprofile','--norc','-s'],{input:wrapped.stdout.replace(/\r$/,'\n'),encoding:'utf8',cwd:temp,windowsHide:true});
    assert.equal(run.status,0,run.stderr);
    const token='0123456789abcdef0123456789abcdef';
    assert(run.stdout.includes(`\x1ePAI:${token}:BEGIN\x1f`));
    assert(run.stdout.includes(`\x1ePAI:${token}:END:${exit}\x1f`),run.stdout);
    assert(run.stdout.includes(expected),run.stdout); checks.push(name);
  }
  assert.equal(readFileSync(join(temp,'output.txt'),'utf8'),'deployment-ok');
  for(const script of ['ip\x1b[A','ip\rreboot','a\0b','x'.repeat(2049),"'".repeat(1500)]) {
    assert.notEqual(spawnSync(emitter,['--wrap'],{input:script,windowsHide:true}).status,0);
  }
  checks.push('control_and_length_rejection');
  const report={passed:true,checks,scope:'Exact C wrapper executed by local POSIX shell; no remote SSH or server deployment.'};
  writeFileSync(resolve(root,'docs/custom-shell-results.json'),JSON.stringify(report,null,2)); console.log(JSON.stringify(report,null,2));
} finally { rmSync(temp,{recursive:true,force:true}); }
