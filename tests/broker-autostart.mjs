// Regression for Windows PowerShell/.NET handle inheritance: the shell must
// exit while the independently spawned Broker remains available.
import {spawnSync} from 'node:child_process';
import {writeFileSync} from 'node:fs';
import {dirname,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
const root=resolve(dirname(fileURLToPath(import.meta.url)),'..');
const adapter=process.argv[2] ? resolve(process.argv[2]) : resolve(root,'putty-ai-rs/target/release/putty-ai-mcp.exe');
const started=performance.now();
const run=spawnSync('powershell.exe',['-NoProfile','-ExecutionPolicy','Bypass','-File',resolve(root,'scripts/check-mcp.ps1'),'-AdapterPath',adapter],
  {windowsHide:true,encoding:'utf8',timeout:12000});
assert.equal(run.status,0,run.stderr || run.error?.message);
const result=JSON.parse(run.stdout); assert(result.passed); assert.equal(result.remote_input_sent,false);
// A second adapter must still reach the independently running Broker.
const next=spawnSync('powershell.exe',['-NoProfile','-ExecutionPolicy','Bypass','-File',resolve(root,'scripts/check-mcp.ps1'),'-AdapterPath',adapter],
  {windowsHide:true,encoding:'utf8',timeout:12000});
assert.equal(next.status,0,next.stderr || next.error?.message); assert(JSON.parse(next.stdout).passed);
const report={passed:true,first_powershell_exit_ms:performance.now()-started,
  checks:['Windows PowerShell exits after initial Broker autostart','independent Broker remains reachable by a second adapter'],
  scope:'Run after isolated tests when no v3 Broker exists. Leaves its idle Broker running.'};
writeFileSync(resolve(root,'docs/broker-autostart-results.json'),JSON.stringify(report,null,2)); console.log(JSON.stringify(report,null,2));
