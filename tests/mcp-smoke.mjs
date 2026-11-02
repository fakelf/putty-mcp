import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {connect} from 'node:net';
import {mkdir,writeFile} from 'node:fs/promises';
import {resolve,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
const root=resolve(dirname(fileURLToPath(import.meta.url)),'..');
const binary=resolve(root,'putty-ai-rs/target/release');
const children=[]; const checks=[];
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
function child(exe,args=[]) { const p=spawn(exe,args,{windowsHide:true,stdio:['pipe','pipe','pipe']}); children.push(p); p.stderr.on('data',()=>{}); return p; }
function lines(p) {
  const queue=[],waiters=[];
  createInterface({input:p.stdout}).on('line',s=>{
    let v; try {v=JSON.parse(s);} catch {throw Error(`non-JSON stdout: ${s.slice(0,80)}`);}
    if(waiters.length) {const w=waiters.shift(); clearTimeout(w.timer); w.resolve(v);} else queue.push(v);
  });
  return async()=>{
    if(queue.length) return queue.shift();
    return new Promise((resolve,reject)=>{
      const w={resolve,timer:setTimeout(()=>reject(Error('stdout timeout')),8000)}; waiters.push(w);
    });
  };
}
async function mcp() {
  const p=child(resolve(binary,'putty-ai-mcp.exe')); const next=lines(p); let id=0;
  async function rpc(method,params) {
    const request=++id;
    p.stdin.write(JSON.stringify({jsonrpc:'2.0',id:request,method,...(params?{params}:{})})+'\n');
    for(;;) { const v=await next(); if(v.id===request) { if(v.error) throw Error(JSON.stringify(v.error)); return v.result; } }
  }
  const initialized=await rpc('initialize',{protocolVersion:'2025-03-26',capabilities:{},clientInfo:{name:'putty-offline-test',version:'1'}});
  assert(initialized.capabilities.tools);
  p.stdin.write(JSON.stringify({jsonrpc:'2.0',method:'notifications/initialized'})+'\n');
  async function tool(name,args={}) { const v=await rpc('tools/call',{name,arguments:args}); if(v.isError) throw Error(v.content[0].text); return JSON.parse(v.content[0].text); }
  return {p,rpc,tool};
}
async function poll(fn,predicate,description,ms=7000) {
  const end=Date.now()+ms; let last;
  while(Date.now()<end) { try { const value=await fn(); if(predicate(value)) return value; } catch(e) {last=e;} await sleep(40); }
  throw Error(`${description}: ${last?.message??'condition not reached'}`);
}
try {
  let broker=child(resolve(binary,'putty-ai-broker.exe')); await sleep(150);
  assert.equal(broker.exitCode,null,'A broker already exists or failed; stop it before isolated tests.');
  const a=await mcp(); const b=await mcp();
  const tools=await a.rpc('tools/list'); assert.equal(tools.tools.length,7);
  await mkdir(resolve(root,'docs'),{recursive:true}); await writeFile(resolve(root,'docs/mcp-tools.json'),JSON.stringify(tools,null,2));
  checks.push('MCP initialize, seven tools, JSON schemas, two real stdio adapters');
  const native=child(resolve(root,'build-ai/Release/test_ai_native.exe')); const nextNative=lines(native);
  const state=await nextNative(); const hex=state.session_hex; assert.equal(hex.length,32);
  const sid=[hex.slice(0,8),hex.slice(8,12),hex.slice(12,16),hex.slice(16,20),hex.slice(20)].join('-');
  const list=await poll(()=>a.tool('list_sessions'),v=>v.sessions.some(s=>s.session_id===sid),'native registration');
  assert.equal(list.sessions.filter(s=>s.session_id===sid).length,1);
  const read=await poll(()=>a.tool('read_terminal',{session_id:sid,cursor:0,max_bytes:8192}),v=>v.output.includes('synthetic-native-output'),'C bridge output');
  const read2=await b.tool('read_terminal',{session_id:sid,cursor:read.next_cursor,max_bytes:8192}); assert(read2.next_cursor>=read.next_cursor);
  checks.push('actual C bridge registration and output through broker and MCP; incremental cursors');
  // No manual menu click: the production offer callback must open consent.
  native.stdin.write('hold\n'); await nextNative();
  await a.tool('acquire_control',{session_id:sid,requested_mode:'diagnostic'});
  await sleep(100); native.stdin.write('ping\n');
  const autoPrompt=await nextNative(); assert.equal(autoPrompt.approval_open,true,'control request did not automatically open consent');
  native.stdin.write('reject\n'); await nextNative(); await sleep(150);
  const rejectedControl=await a.tool('acquire_control',{session_id:sid,requested_mode:'diagnostic'});
  assert.equal(rejectedControl.status,'rejected_locally','rejected client automatically re-opened consent');
  await a.tool('release_control',{session_id:sid});
  const afterReject=await b.tool('acquire_control',{session_id:sid,requested_mode:'diagnostic'});
  assert.equal(afterReject.status,'pending_local_approval','rejected client left a stale pending lock');
  native.stdin.write('allow\n'); await nextNative();
  await poll(()=>b.tool('acquire_control',{session_id:sid,requested_mode:'diagnostic'}),v=>v.status==='granted','second client after local rejection');
  await b.tool('release_control',{session_id:sid}); await sleep(100);
  checks.push('production automatically opens control consent; rejecting it clears the broker pending lock and permits a new client');
  native.stdin.write('hold\n'); const held=await nextNative();
  const pending=await a.tool('acquire_control',{session_id:sid,requested_mode:'diagnostic'}); assert.equal(pending.status,'pending_local_approval');
  await sleep(100); native.stdin.write('ping\n'); const duringApproval=await nextNative(); assert(duringApproval.ticks>held.ticks);
  checks.push('real modeless consent remains pending while native terminal event ticks continue');
  native.stdin.write('allow\n'); await nextNative();
  await poll(()=>a.tool('acquire_control',{session_id:sid,requested_mode:'diagnostic'}),v=>v.status==='granted','synthetic local approval');
  await assert.rejects(()=>b.tool('acquire_control',{session_id:sid,requested_mode:'diagnostic'}));
  const started=performance.now();
  const task=await a.tool('submit_command',{session_id:sid,command:'uname -a',execution_timeout_ms:5000,request_id:'native-smoke'});
  assert.equal(task.status,'queued'); const submitMs=performance.now()-started;
  const completed=await poll(()=>a.tool('get_task_status',{task_id:task.task_id,max_output_bytes:8192}),v=>v.status==='completed','native synthetic command completion');
  assert.equal(completed.exit_code,0); assert(completed.output.includes('SIMULATED uname result'));
  checks.push('lease bound to MCP connection; immediate task submission; C bridge markers and completion');
  native.stdin.write('human\n'); await nextNative(); await sleep(100);
  await assert.rejects(()=>a.tool('submit_command',{session_id:sid,command:'uptime',execution_timeout_ms:5000,request_id:'revoked'}));
  checks.push('human activity revokes control');
  // Custom mode is a separately approved capability; diagnostic never broadens silently.
  await poll(()=>a.tool('acquire_control',{session_id:sid,requested_mode:'diagnostic'}),v=>v.status==='granted','diagnostic reapproval');
  await assert.rejects(()=>a.tool('submit_command',{session_id:sid,command:'ip',execution_timeout_ms:5000,request_id:'diagnostic-cannot-ip'}));
  await a.tool('release_control',{session_id:sid});
  await poll(()=>a.tool('acquire_control',{session_id:sid,requested_mode:'custom'}),v=>v.status==='granted','custom local approval');
  await assert.rejects(()=>b.tool('acquire_control',{session_id:sid,requested_mode:'custom'}));
  for(const [index,command] of ['ip','ip -br addr | cat',"printf '%s\\n' '部署测试'\nprintf '%s' \"quoted 'text'\""].entries()) {
    const t=await a.tool('submit_command',{session_id:sid,command,execution_timeout_ms:10000,request_id:`custom-${index}`});
    const v=await poll(()=>a.tool('get_task_status',{task_id:t.task_id}),v=>v.status==='completed','custom synthetic completion');
    assert.equal(v.exit_code,0); assert(v.output.includes('SIMULATED custom result'));
  }
  await assert.rejects(()=>a.tool('submit_command',{session_id:sid,command:'ip\rreboot',execution_timeout_ms:10000,request_id:'terminal-control'}));
  native.stdin.write('hold\n'); await nextNative();
  const rejected=await a.tool('submit_command',{session_id:sid,command:'ip',execution_timeout_ms:10000,request_id:'human-while-approval'});
  await sleep(100); native.stdin.write('human\n'); await nextNative();
  const cancelled=await poll(()=>a.tool('get_task_status',{task_id:rejected.task_id}),v=>['cancelled','unknown'].includes(v.status),'human cancelled custom approval');
  assert.equal(cancelled.exit_code,null);
  native.stdin.write('allow\n'); await nextNative();
  await poll(()=>a.tool('acquire_control',{session_id:sid,requested_mode:'custom'}),v=>v.status==='granted','custom reapproval for revoke race');
  native.stdin.write('hold\n'); await nextNative();
  const cancelRace=await a.tool('submit_command',{session_id:sid,command:'ip',execution_timeout_ms:10000,request_id:'release-during-consent'});
  await sleep(100); await a.tool('release_control',{session_id:sid}); await sleep(100);
  native.stdin.write('allow\n'); await nextNative();
  const revokeState=await a.tool('get_task_status',{task_id:cancelRace.task_id});
  assert.notEqual(revokeState.status,'completed'); assert.equal(revokeState.exit_code,null);
  checks.push('custom requires separate local grant, permits ip/pipes/UTF-8 scripts, rejects terminal controls; human input cancels pending command');
  native.stdin.write('flood\n'); const flooded=await nextNative(); assert(flooded.dropped>0);
  await poll(()=>a.tool('read_terminal',{session_id:sid,cursor:0}),v=>v.lost_bytes>0,'native ring overflow');
  checks.push('native ring overflow discloses missing bytes without waiting for MCP');
  // Native event loop must answer while its IPC worker is offline.
  broker.kill(); await new Promise(r=>broker.once('exit',r));
  const pingStart=performance.now(); native.stdin.write('ping\n'); const ping=await nextNative(); const offlinePingMs=performance.now()-pingStart;
  assert(ping.ticks>state.ticks);
  broker=child(resolve(binary,'putty-ai-broker.exe')); await sleep(100);
  await poll(()=>a.tool('list_sessions'),v=>v.sessions.some(s=>s.session_id===sid),'native and adapter recovery',10000);
  checks.push('broker killed: native GUI heartbeat survives; native and MCP reconnect without remote reconnection');
  await poll(()=>b.tool('list_sessions'),v=>v.sessions.some(s=>s.session_id===sid),'second adapter recovery');
  a.p.kill(); await sleep(100); const alive=await b.tool('read_terminal',{session_id:sid}); assert(alive.next_cursor>0);
  checks.push('MCP adapter crash does not interrupt another reader or native output');
  native.stdin.write('off\n'); await nextNative();
  await poll(()=>b.tool('list_sessions'),v=>!v.sessions.some(s=>s.session_id===sid),'disabled native unregister');
  await assert.rejects(()=>b.tool('read_terminal',{session_id:sid}));
  native.stdin.write('on\n'); const fresh=await nextNative(); assert.notEqual(fresh.session_hex,hex);
  checks.push('disable closes IPC, rejects old identity; re-enable creates a new random identity');
  const report={passed:true,checks,submit_ms:submitMs,native_event_loop_ping_ms_while_broker_offline:offlinePingMs,
    native_dropped_bytes:flooded.dropped,note:'synthetic frontend auto-approves synthetic commands; no SSH/bastion/real remote command used'};
  await writeFile(resolve(root,'docs/mcp-smoke-results.json'),JSON.stringify(report,null,2));
  console.log(JSON.stringify(report,null,2));
} finally {
  for(const p of children.reverse()) { if(p.exitCode===null) p.kill(); }
}
