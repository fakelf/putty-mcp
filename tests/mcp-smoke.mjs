import {execFileSync,spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {mkdir,writeFile} from 'node:fs/promises';
import {resolve,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
const root=resolve(dirname(fileURLToPath(import.meta.url)),'..');
const mcpPath=resolve(process.env.PUTTY_SMOKE_MCP ?? resolve(root,'putty-ai-rs/target/release/putty-ai-mcp.exe'));
const nativePath=resolve(process.env.PUTTY_SMOKE_NATIVE ?? resolve(root,'build-ai/Release/test_ai_native.exe'));
const reportDir=resolve(process.env.PUTTY_SMOKE_REPORT_DIR ?? resolve(root,'docs'));
const connectionReport=session=>execFileSync(mcpPath,
  ['--connection-report',session,'--text'],{encoding:'utf8',windowsHide:true,timeout:8000});
const children=[]; const checks=[];
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
function child(exe,args=[],env=process.env) { const p=spawn(exe,args,{env,windowsHide:true,stdio:['pipe','pipe','pipe']}); children.push(p); p.stderr.on('data',()=>{}); return p; }
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
async function mcp(source) {
  const env={...process.env};
  delete env.PUTTY_MCP_SOURCE;
  if(source) env.PUTTY_MCP_SOURCE=source;
  const p=child(mcpPath,[],env); const next=lines(p); let id=0;
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
  const native=child(nativePath);
  const nextNative=lines(native);
  const state=await nextNative();
  assert.equal(state.online,0,'expected an isolated test with PuTTY starting first');
  const hex=state.session_hex;
  assert.equal(hex.length,32);
  const sid=[hex.slice(0,8),hex.slice(8,12),hex.slice(12,16),hex.slice(16,20),hex.slice(20)].join('-');
  assert.match(connectionReport(sid),/Broker: not reachable/);
  let broker=child(mcpPath,['--broker']);
  await sleep(150);
  assert.equal(broker.exitCode,null,'A v4 broker already exists or failed; stop it before isolated tests.');
  const a=await mcp('Harness test');
  const b=await mcp();
  const tools=await a.rpc('tools/list');
  assert.deepEqual(tools.tools.map(t=>t.name).sort(),
    ['attach_terminal','cancel_task','close_child_session','get_task_status','interrupt_task','list_sessions','open_child_session','read_task_screen','read_terminal','request_task_action','start_interactive','submit_command']);
  const namedKeys={
    escape:'Escape',
    ...Object.fromEntries(Array.from({length:12},(_,i)=>[`f${i+1}`,`F${i+1}`])),
    ctrl_q:'Ctrl+Q',ctrl_s:'Ctrl+S',ctrl_d:'Ctrl+D',ctrl_z:'Ctrl+Z',
    backspace:'Backspace',delete:'Delete',up:'Up',down:'Down',left:'Left',right:'Right',
    home:'Home',end:'End',page_up:'Page Up',page_down:'Page Down',insert:'Insert',
    tab:'Tab',shift_tab:'Shift+Tab',enter:'Enter',undo:'Undo',redo:'Redo',insert_mode:'Enter insert mode',
  };
  const keySchema=tools.tools.find(t=>t.name==='request_task_action').inputSchema;
  assert.deepEqual(keySchema.$defs.Action.enum,['quit',...Object.keys(namedKeys),'discard_and_quit','insert_text','save','press_key','type_text','paste_text','detach']);
  await mkdir(reportDir,{recursive:true});
  await writeFile(resolve(reportDir,'mcp-tools.json'),JSON.stringify(tools,null,2));
  checks.push('Twelve live MCP tools with machine-readable schemas; both adapters run without arguments');
  const list=await poll(()=>a.tool('list_sessions'),v=>v.sessions.some(s=>s.session_id===sid),'PuTTY-first registration');
  assert.equal(list.sessions.filter(s=>s.session_id===sid).length,1);
  assert(list.sessions.find(s=>s.session_id===sid).available_capabilities.includes('interactive_vi'));
  assert(list.sessions.find(s=>s.session_id===sid).available_capabilities.includes('interactive_vi_file'));
  const registeredReport=connectionReport(sid);
  assert.match(registeredReport,/MCP hosts registered: 2/);
  assert.match(registeredReport,/Current PuTTY window: registered/);
  checks.push('Passive connection report distinguishes missing broker, live MCP hosts and exact window registration');
  const read=await poll(()=>a.tool('read_terminal',{session_id:sid,cursor:0,max_bytes:8192}),v=>v.output.includes('synthetic-native-output'),'C bridge output');
  const read2=await b.tool('read_terminal',{session_id:sid,cursor:read.next_cursor,max_bytes:8192});
  assert(read2.next_cursor>=read.next_cursor);
  checks.push('PuTTY-first startup, C output and incremental reading through two MCP connections');

  async function nativeInput(input) {
    native.stdin.write(input+'\n');
    return nextNative();
  }
  const taskStatus=t=>a.tool('get_task_status',{task_id:t.task_id});
  const attach=(request_id,session_id=sid,client=a)=>client.tool('attach_terminal',{session_id,request_id});
  const screen=(task,client=a,after_frame)=>client.tool('read_task_screen',{task_id:task.task_id,after_frame});
  const action=(task,request_id,input,client=a)=>client.tool('request_task_action',{task_id:task.task_id,request_id,...input});
  const sent=(task,request_id,client=a)=>poll(()=>client.tool('get_task_status',{task_id:task.task_id}),
    s=>s.action_request_id===request_id && s.action_status==='sent','foreground input accepted');
  const submit=(request_id,command='ip -br addr | cat',execution_timeout_ms=5000,client=a)=>
    client.tool('submit_command',{session_id:sid,command,execution_timeout_ms,request_id});
  const prompt=(interrupt=false)=>poll(()=>nativeInput('ping'),
    v=>v.approval_open && v.interrupt_prompt===interrupt,'expected local approval dialog');

  const held=await nativeInput('hold');
  await assert.rejects(()=>a.tool('submit_command',{session_id:sid,command:'ip',execution_timeout_ms:1000,request_id:'spoof',source:'Fake'}));
  const started=performance.now();
  const task=await submit('single-approval','display current-configuration',1000);
  const submitMs=performance.now()-started;
  assert.equal(task.status,'queued');
  assert.equal(task.requires_local_command_approval,true);
  assert.equal(task.source,'Harness test (local configuration)');
  const approval=await prompt();
  assert(approval.ticks>held.ticks && approval.configured_source_visible);
  await assert.rejects(()=>submit('another-client','uptime',1000,b),/unresolved command/);
  await assert.rejects(()=>b.tool('get_task_status',{task_id:task.task_id}),/another MCP client/);
  await assert.rejects(()=>a.tool('interrupt_task',{task_id:task.task_id}),/current running task/);
  await sleep(1300);
  assert.equal((await taskStatus(task)).status,'queued');
  await nativeInput('allow');
  const completed=await poll(()=>taskStatus(task),v=>v.status==='completed','single approval completed command');
  assert.equal(completed.exit_code,null);
  assert.equal(completed.completion,'prompt_observed');
  assert(completed.output.includes('sysname branch-switch-lab'));
  assert.equal(completed.command,'display current-configuration');
  assert(!completed.output.includes('PAI:'));
  const transcript=await a.tool('get_task_status',{task_id:task.task_id,output_view:'transcript'});
  assert(transcript.output.includes('display current-configuration'));
  assert(!transcript.output.includes('PAI:') && !transcript.output.includes('eval '));
  checks.push('Native fixture accepts only exact device command bytes and Enter; task output and transcript contain no injected wrapper');
  checks.push('One approval shows configured source and target; pending approval outlives the one-second execution budget; no second client can replace it');

  await nativeInput('hold');
  const rejected=await submit('reject-once');
  await prompt();
  await nativeInput('reject');
  await poll(()=>taskStatus(rejected),v=>v.status==='cancelled','local rejection');
  assert.equal((await submit('reject-once')).task_id,rejected.task_id);
  assert.equal((await nativeInput('ping')).approval_open,false);

  const bash=await submit('device-prompt','synthetic-long-task',20000);
  await nativeInput('allow');
  await poll(()=>taskStatus(bash),v=>v.status==='running','running device CLI task');
  await nativeInput('hold');
  await a.tool('interrupt_task',{task_id:bash.task_id});
  await prompt(true);
  await nativeInput('allow');
  const returned=await poll(()=>taskStatus(bash),
    v=>v.interrupt_status==='sent' && v.shell_prompt_observed,'Ctrl+C followed by the device prompt');
  assert.equal(returned.exit_code,null);
  assert.equal(returned.status,'completed');
  assert.equal(returned.completion,'prompt_observed');
  checks.push('Native Ctrl+C sequence correlates prompt return without inventing an exit code');
  await nativeInput('hold');
  const cancelled=await submit('cancel-pending');
  await prompt();
  await a.tool('cancel_task',{task_id:cancelled.task_id});
  await poll(()=>taskStatus(cancelled),v=>v.status==='cancelled','cancel pending approval');
  const takeover=await submit('human-takeover');
  await prompt();
  await nativeInput('human');
  await poll(()=>taskStatus(takeover),v=>v.status==='cancelled','human cancelled approval');
  checks.push('Reject, cancel, and manual input invalidate the exact pending command; repeated request ID cannot reopen it');

  const gone=await mcp();
  const disconnected=await submit('client-disconnect','ip',5000,gone);
  assert.equal(disconnected.source,'putty-offline-test (reported by MCP client)');
  await prompt();
  gone.p.kill();
  await poll(()=>a.tool('list_sessions'),v=>v.sessions.find(s=>s.session_id===sid)?.command_state==='idle',
    'adapter disconnect dismisses its pending approval');
  assert.equal((await nativeInput('ping')).approval_open,false);

  const script=await submit('unicode-script',"printf '%s\\n' 'hello'\nprintf '%s' \"quoted 'text'\" | cat");
  await prompt();
  await nativeInput('allow');
  await poll(()=>taskStatus(script),v=>v.status==='completed','multiline command after rejection');
  await assert.rejects(()=>submit('terminal-control','ip\rreboot'),/control characters/);

  const foreground=await submit('long-task','synthetic-long-task',20000);
  await poll(()=>taskStatus(foreground),v=>v.status==='running','running foreground task');
  await nativeInput('hold');
  await assert.rejects(()=>b.tool('interrupt_task',{task_id:foreground.task_id}),/another MCP client/);
  const interrupt=()=>a.tool('interrupt_task',{task_id:foreground.task_id});
  assert.equal((await interrupt()).interrupt_status,'pending_approval');
  const interruptApproval=await prompt(true);
  assert(interruptApproval.configured_source_visible);
  assert.equal((await interrupt()).interrupt_status,'pending_approval');
  await nativeInput('allow');
  const stopped=await poll(()=>taskStatus(foreground),
    v=>v.interrupt_status==='sent' && v.completion==='prompt_observed','approved synthetic Ctrl+C');
  assert.equal(stopped.exit_code,null);
  assert.equal(stopped.remote_process_stopped,false);
  assert.equal((await interrupt()).interrupt_status,'sent');
  assert.equal((await nativeInput('ping')).approval_open,false);

  const doneDuringApproval=await submit('finishes-before-interrupt','synthetic-long-task',20000);
  await poll(()=>taskStatus(doneDuringApproval),v=>v.status==='running','running task before finish');
  await nativeInput('hold');
  await a.tool('interrupt_task',{task_id:doneDuringApproval.task_id});
  await prompt(true);
  await nativeInput('finish');
  const naturallyDone=await poll(()=>taskStatus(doneDuringApproval),
    v=>v.status==='completed' && v.interrupt_status==='not_sent','finished task revokes interrupt');
  assert.equal(naturallyDone.exit_code,null);
  await nativeInput('allow');
  assert.equal((await nativeInput('ping')).approval_open,false);
  checks.push('One-shot Ctrl+C requires local approval and same task owner; completed tasks revoke pending interrupts; sent key and observed result are distinct');

  for (const [command,expected_prompt] of [['system-view','[branch-switch-lab]'],['return','<branch-switch-lab>']]) {
    const changed=await a.tool('submit_command',{
      session_id:sid,command,expected_prompt,request_id:`cli-mode-${command}`,execution_timeout_ms:5000,
    });
    const outcome=await poll(()=>taskStatus(changed),v=>v.status==='completed','observed changed CLI prompt');
    assert.equal(outcome.completion,'prompt_observed');
    assert.equal(outcome.exit_code,null);
  }
  checks.push('A literal expected_prompt tracks the changed device mode without sending extra input');

  await nativeInput('pager-on');
  const pager=await submit('h3c-pager','display current-configuration',30000);
  const firstPagerFrame=await poll(()=>screen(pager),s=>s.frame?.text.includes('----more----'),'foreground pager screen');
  assert.equal(firstPagerFrame.frame.columns,80);
  assert.equal(firstPagerFrame.frame.rows,24);
  assert.deepEqual(firstPagerFrame.frame.cursor,{column:1,row:2});
  assert.equal(firstPagerFrame.untrusted_terminal_data,true);
  await assert.rejects(()=>screen(pager,b),/another MCP client/);
  await assert.rejects(()=>action(pager,'foreign',{action:'press_key',key:'space'},b),/another MCP client/);
  const space={action:'press_key',key:'space'};
  await nativeInput('hold');
  assert.equal((await action(pager,'stale-space',{...space,if_frame:firstPagerFrame.frame.frame_id})).action_status,'pending_approval');
  assert.equal((await prompt()).configured_source_visible,true);
  await nativeInput('repaint');
  const repainted=await poll(()=>screen(pager,a,firstPagerFrame.frame.frame_id),s=>s.frame?.frame_id>firstPagerFrame.frame.frame_id,'new pager frame');
  await nativeInput('allow');
  await poll(()=>taskStatus(pager),s=>s.action_request_id==='stale-space' && s.action_status==='not_sent','stale pager approval fenced');
  assert.equal((await nativeInput('ping')).terminal_input_count,0);

  for (const [name,input,expect] of [
    ['space',space,'page 2'],
    ['enter',{action:'press_key',key:'enter'},'page 3'],
    ['stop',{action:'press_key',key:'c',modifiers:['ctrl']},null],
  ]) {
    const visible=name==='space'?repainted:await screen(pager);
    const args={...input,if_frame:visible.frame.frame_id};
    await nativeInput('hold');
    const asked=await action(pager,name,args);
    assert.equal(asked.action_status,'pending_approval');
    await prompt();
    assert.equal((await action(pager,name,args)).action_id,asked.action_id);
    await nativeInput('allow');
    await sent(pager,name);
    if(expect) await poll(()=>screen(pager),s=>s.frame?.text.includes(expect),'pager state after one key');
    assert.equal((await action(pager,name,args)).action_status,'sent');
  }
  const stoppedPager=await poll(()=>taskStatus(pager),s=>s.completion==='prompt_observed','Ctrl+C returns H3C prompt');
  assert.equal(stoppedPager.exit_code,null);
  assert(stoppedPager.output.includes('page 2 content') && stoppedPager.output.includes('next line'));
  assert.equal((await nativeInput('ping')).terminal_input_count,3);
  checks.push('Foreground H3C fixture receives one Space, Enter and Ctrl+C under separate approvals, guards stale rendered frames and returns its original prompt');

  await nativeInput('hold');
  const foregroundAttach=await attach('manual-attach');
  assert.equal(foregroundAttach.status,'queued');
  assert.equal(foregroundAttach.requires_local_command_approval,true);
  assert.equal((await attach('manual-attach')).task_id,foregroundAttach.task_id);
  await prompt();
  await nativeInput('allow');
  await poll(()=>taskStatus(foregroundAttach),s=>s.status==='running','attach current foreground without sending input');
  assert.equal((await nativeInput('ping')).terminal_input_count,3);
  for (const [request_id,kind,body] of [
    ['foreground-type','type_text','中文编辑'],
    ['foreground-paste','paste_text','第一行\n第二行'],
  ]) {
    await nativeInput('hold');
    assert.equal((await action(foregroundAttach,request_id,{action:kind,text:body})).action_status,'pending_approval');
    await prompt();
    await nativeInput('allow');
    await sent(foregroundAttach,request_id);
    await poll(()=>screen(foregroundAttach),s=>s.frame?.text.includes(body),'Unicode input visible in foreground');
  }
  const priorInputCount=(await nativeInput('hold')).terminal_input_count;
  await action(foregroundAttach,'revoked-text',{action:'type_text',text:'revoked'});
  await prompt();
  await nativeInput('human');
  await nativeInput('allow');
  await poll(()=>taskStatus(foregroundAttach),s=>s.status!=='running','manual input detaches foreground');
  assert.equal((await nativeInput('ping')).terminal_input_count,priorInputCount);
  const reattached=await attach('after-manual-input');
  await poll(()=>taskStatus(reattached),s=>s.status==='running','explicit reattachment');
  const detach={action:'detach'};
  await action(reattached,'release',detach);
  await poll(()=>action(reattached,'release',detach),s=>s.action_status==='detached','detach without remote input');
  assert.equal((await nativeInput('ping')).terminal_input_count,priorInputCount);
  checks.push('Attach and detach are owner-bound, typed Unicode and multiline paste are individually approved, and human takeover revokes pending input');

  const startTop=async request_id=>{
    await nativeInput('hold');
    const t=await a.tool('start_interactive',{session_id:sid,program:'top',request_id});
    assert.equal(t.status,'queued');
    assert.equal((await prompt()).top_prompt,'start');
    await nativeInput('allow');
    await poll(()=>taskStatus(t),v=>v.status==='running','running top task');
    return t;
  };
  const top=await startTop('top-quit');
  await assert.rejects(()=>submit('main-command-during-top'),/unresolved command/);
  await assert.rejects(()=>b.tool('read_task_screen',{task_id:top.task_id}),/another MCP client/);
  const first=await poll(()=>a.tool('read_task_screen',{task_id:top.task_id}),v=>v.frame,'first top frame');
  assert.match(first.frame.text,/top - sample/);
  assert.deepEqual(first.frame.cursor,{column:0,row:1});
  assert.equal(first.frame.paste_available,false);
  await assert.rejects(()=>a.tool('request_task_action',
    {task_id:top.task_id,action:'insert_text',request_id:'wrong-profile',text:'blocked'}),/only available for vi/);
  const next=await poll(()=>a.tool('read_task_screen',{task_id:top.task_id,after_frame:first.frame.frame_id}),
    v=>v.frame,'next top frame');
  assert(next.frame.frame_id>first.frame.frame_id && next.frame.text!==first.frame.text);
  await nativeInput('hold');
  const quit=t=>a.tool('request_task_action',{task_id:t.task_id,action:'quit'});
  assert.equal((await quit(top)).quit_status,'pending_approval');
  assert.equal((await prompt()).top_prompt,'quit');
  await nativeInput('allow');
  const topDone=await poll(()=>taskStatus(top),v=>v.channel_closed && v.quit_status==='sent','top quit and SSH exit');
  assert.equal(topDone.exit_code,0);
  assert.equal((await quit(top)).quit_status,'sent');
  assert.equal((await nativeInput('ping')).approval_open,false);

  const reportsExitBeforeClose=await startTop('top-exit-status-before-close');
  await nativeInput('top-exit-report');
  const closing=await poll(()=>taskStatus(reportsExitBeforeClose),v=>v.status==='closing','SSH exit status before close');
  assert.equal(closing.exit_code,0);
  assert.equal(closing.channel_closed,false);
  await assert.rejects(()=>quit(reportsExitBeforeClose),/not available for AI input/);
  await nativeInput('top-finish');
  await poll(()=>taskStatus(reportsExitBeforeClose),v=>v.channel_closed,'SSH closes after exit status');
  checks.push('Standalone SSH exit status is accepted before channel close and blocks new keys');

  const closesDuringApproval=await startTop('top-ends-before-quit');
  await nativeInput('hold');
  await quit(closesDuringApproval);
  await prompt();
  await nativeInput('top-finish');
  await poll(()=>taskStatus(closesDuringApproval),v=>v.channel_closed && v.quit_status==='not_sent','stale top approval revoked');
  assert.equal((await nativeInput('ping')).approval_open,false);

  const topInterrupted=await startTop('top-interrupt');
  await nativeInput('hold');
  await a.tool('interrupt_task',{task_id:topInterrupted.task_id});
  assert.equal((await prompt()).top_prompt,'interrupt');
  await nativeInput('allow');
  const signalled=await poll(()=>taskStatus(topInterrupted),v=>v.channel_closed && v.interrupt_status==='sent','top interrupted');
  assert.equal(signalled.exit_signal_received,true);
  assert.equal(signalled.exit_code,null);
  const taken=await startTop('top-manual-takeover');
  await nativeInput('hold');
  await quit(taken);
  assert.equal((await prompt()).top_prompt,'quit');
  await nativeInput('human');
  await poll(()=>taskStatus(taken),v=>v.taken_over && v.quit_status==='not_sent','top manual takeover');
  assert.equal((await a.tool('read_task_screen',{task_id:taken.task_id})).frame,null);
  await nativeInput('top-finish');
  await poll(()=>taskStatus(taken),v=>v.channel_closed,'finish taken-over top');
  await nativeInput('allow');
  checks.push('Dedicated top protocol separates rendered frames, key dispatch, channel closure, exit status and exit signal');
  checks.push('Top ownership is enforced; a channel closing during approval revokes the pending key');

  await nativeInput('hold');
  const viStart={session_id:sid,program:'vi',request_id:'start-vi'};
  const vi=await a.tool('start_interactive',viStart);
  assert.equal(vi.status,'queued');
  assert.equal(vi.program,'vi');
  assert.equal(vi.command,"exec vi -Nu NONE -i NONE -n -R -Z -c 'set encoding=utf-8 backspace=indent,eol,start'");
  assert.equal((await prompt()).top_prompt,'vi_start');
  assert.equal((await a.tool('start_interactive',viStart)).task_id,vi.task_id);
  await assert.rejects(()=>a.tool('start_interactive',{...viStart,program:'top'}),/different program/);
  await nativeInput('allow');
  await poll(()=>taskStatus(vi),v=>v.status==='running','dedicated vi PTY running');
  const requestKey=(action,request_id,client=a)=>client.tool('request_task_action',
    {task_id:vi.task_id,action,request_id});
  await assert.rejects(()=>requestKey('escape',undefined),/request_id/);
  await assert.rejects(()=>requestKey('f13','unsupported'));
  await assert.rejects(()=>requestKey('escape','foreign',b),/another MCP client/);
  let sentKeys=0;
  for (const [action,label] of Object.entries(namedKeys)) {
    const other=action==='f2'?'f1':'f2';
    await nativeInput('hold');
    const id=`vi-${action}`;
    const pending=await requestKey(action,id);
    assert.equal(pending.key_status,'pending_approval');
    assert.equal(pending.key_request_id,id);
    assert.equal((await prompt()).top_prompt,'special_key');
    assert.equal((await requestKey(action,id)).key_status,'pending_approval');
    await assert.rejects(()=>requestKey(other,id),/different key/);
    await assert.rejects(()=>requestKey(other,`${id}-another`),/awaiting a decision/);
    await nativeInput('allow');
    const delivered=await poll(()=>taskStatus(vi),v=>v.key_status==='sent' && v.key_request_id===id,
      `approved ${action} dispatched`);
    assert.equal(delivered.channel_closed,false);
    await poll(()=>a.tool('read_task_screen',{task_id:vi.task_id}),
      v=>v.frame?.text.split('\n').includes(`vi key: ${label}`),`${action} reached synthetic native frontend`);
    assert.equal((await requestKey(action,id)).key_status,'sent');
    const afterReplay=await nativeInput('ping');
    assert.equal(afterReplay.approval_open,false);
    assert.equal(afterReplay.special_key_count,++sentKeys);
  }
  assert.equal((await requestKey('escape','vi-escape')).key,'escape');
  await nativeInput('hold');
  assert.equal((await requestKey('ctrl_d','vi-reject')).key_status,'pending_approval');
  assert.equal((await prompt()).top_prompt,'special_key');
  await nativeInput('reject');
  await poll(()=>taskStatus(vi),v=>v.key_status==='not_sent' && v.key_request_id==='vi-reject','rejected key is not sent');
  assert.equal((await requestKey('ctrl_d','vi-reject')).key_status,'not_sent');
  assert.equal((await nativeInput('ping')).special_key_count,sentKeys);
  const paste=extra=>a.tool('request_task_action',
    {task_id:vi.task_id,action:'insert_text',request_id:'vi-text',text:'draft :qa!\n\t雪',...extra});
  await assert.rejects(()=>paste({text:'unsafe\x1b[201~'}),/UTF-8 bytes/);
  await assert.rejects(()=>paste({text:'x'.repeat(1025)}),/UTF-8 bytes/);
  await assert.rejects(()=>paste({text:undefined}),/requires text/);
  await nativeInput('hold');
  assert.equal((await paste()).key_status,'pending_approval');
  assert.equal((await prompt()).top_prompt,'vi_insert_text');
  assert.equal((await paste()).key_status,'pending_approval');
  await assert.rejects(()=>paste({text:'different'}),/different key or text/);
  assert.equal((await nativeInput('ping')).text_insert_count,0);
  await nativeInput('allow');
  const pasted=await poll(()=>taskStatus(vi),v=>v.key_status==='sent' && v.key_request_id==='vi-text',
    'approved text dispatch');
  assert.equal(pasted.key,'insert_text');
  assert.equal(pasted.channel_closed,false);
  assert(!JSON.stringify(pasted).includes('draft :qa!'));
  const textScreen=await poll(()=>a.tool('read_task_screen',{task_id:vi.task_id}),
    v=>v.frame?.text.includes('vi text fixture delivered'),'exact text reached synthetic native frontend');
  assert.equal(textScreen.frame.paste_available,true);
  assert.deepEqual(textScreen.frame.cursor,{column:0,row:1});
  assert.equal((await paste()).key_status,'sent');
  assert.equal((await nativeInput('ping')).text_insert_count,1);
  await nativeInput('hold');
  assert.equal((await quit(vi)).quit_status,'pending_approval');
  assert.equal((await prompt()).top_prompt,'vi_quit');
  await nativeInput('allow');
  const viDone=await poll(()=>taskStatus(vi),v=>v.channel_closed && v.quit_status==='sent','vi quit channel closure');
  assert.equal(viDone.exit_code,0);
  await assert.rejects(()=>requestKey('ctrl_z','vi-after-exit'),/not available for AI input/);
  await nativeInput('hold');
  const scratch=await a.tool('start_interactive',{...viStart,request_id:'vi-discard-start'});
  assert.equal((await prompt()).top_prompt,'vi_start');
  await nativeInput('allow');
  await poll(()=>taskStatus(scratch),v=>v.status==='running','second dedicated vi running');
  await assert.rejects(()=>a.tool('request_task_action',
    {task_id:scratch.task_id,action:'save',request_id:'scratch-save'}),/file|path/i);
  await nativeInput('hold');
  const discard=await a.tool('request_task_action',
    {task_id:scratch.task_id,action:'discard_and_quit',request_id:'discard'});
  assert.equal(discard.key_status,'pending_approval');
  assert.equal((await prompt()).top_prompt,'vi_discard');
  await nativeInput('allow');
  await poll(()=>taskStatus(scratch),v=>v.channel_closed && v.key_status==='sent','approved explicit vi discard');
  checks.push('Vi startup and quit have separate local approvals; named keys are allowlisted, one-shot per request and tied to the active owned PTY');
  checks.push('Vi text approval shows exact multiline Unicode and source; invalid input and cross-profile use are rejected; duplicate requests send once; explicit discard has its own prompt');

  const path="/home/fixture/test 'é'.sh";
  const openFile={session_id:sid,program:'vi',path,request_id:'start-vi-file'};
  for (const invalid of ['~/test.sh','/home/../test.sh','//test.sh','/home/test.sh\nmore','/home/']) {
    await assert.rejects(()=>a.tool('start_interactive',{...openFile,path:invalid}),/path/i);
  }
  await assert.rejects(()=>a.tool('start_interactive',{...openFile,program:'top'}),/path|vi/i);
  await nativeInput('hold');
  const file=await a.tool('start_interactive',openFile);
  assert.equal(file.path,path);
  assert.equal(file.status,'queued');
  assert.equal((await prompt()).top_prompt,'vi_file_open');
  assert.equal((await a.tool('start_interactive',openFile)).task_id,file.task_id);
  await assert.rejects(()=>a.tool('start_interactive',{...openFile,path:'/home/fixture/other.sh'}),/different|reused/i);
  await nativeInput('allow');
  await poll(()=>taskStatus(file),v=>v.status==='running','approved file is open in synthetic vi');
  const saveFile=(request_id,client=a)=>client.tool('request_task_action',{task_id:file.task_id,action:'save',request_id});
  await assert.rejects(()=>saveFile(undefined),/request_id/);
  await assert.rejects(()=>saveFile('other-client',b),/another MCP client/);
  await nativeInput('hold');
  assert.equal((await saveFile('declined')).key_status,'pending_approval');
  assert.equal((await prompt()).top_prompt,'vi_file_save');
  await nativeInput('reject');
  await poll(()=>taskStatus(file),v=>v.key_status==='not_sent' && v.key_request_id==='declined','save rejected');
  assert.equal((await saveFile('declined')).key_status,'not_sent');
  assert.equal((await nativeInput('ping')).file_save_count,0);
  await nativeInput('hold');
  assert.equal((await saveFile('save-once')).key_status,'pending_approval');
  assert.equal((await prompt()).top_prompt,'vi_file_save');
  assert.equal((await saveFile('save-once')).key_status,'pending_approval');
  await nativeInput('allow');
  const saved=await poll(()=>taskStatus(file),v=>v.key_status==='sent' && v.key_request_id==='save-once','approved save dispatch');
  assert.equal(saved.path,path);
  assert.equal(saved.key,'save');
  assert.equal(saved.channel_closed,false);
  await poll(()=>a.tool('read_task_screen',{task_id:file.task_id}),
    v=>v.frame?.text.includes('vi file save dispatched'),'save reached the synthetic frontend');
  assert.equal((await saveFile('save-once')).key_status,'sent');
  assert.equal((await nativeInput('ping')).file_save_count,1);
  await nativeInput('hold');
  assert.equal((await quit(file)).quit_status,'pending_approval');
  assert.equal((await prompt()).top_prompt,'vi_quit');
  await nativeInput('allow');
  await poll(()=>taskStatus(file),v=>v.channel_closed && v.quit_status==='sent','file vi normal quit');
  await assert.rejects(()=>saveFile('closed'),/not available/);
  checks.push('Vi file open and save show the exact approved path; invalid paths, scratch saves, foreign clients, rejection, replay and closed tasks are enforced');

  const findSession=async (id,client=a)=>(await client.tool('list_sessions')).sessions.find(s=>s.session_id===id);
  const openChild=(request_id,client=a,session_id=sid)=>client.tool('open_child_session',{session_id,request_id});
  const closeChild=(session_id,request_id,client=a)=>client.tool('close_child_session',{session_id,request_id});
  assert.equal((await findSession(sid)).mode,'normal');
  assert.equal((await findSession(sid)).approval_mode,'per_action');
  await assert.rejects(()=>openChild('normal-mode'),/mode|child/i);
  await nativeInput('hold');
  const oldNormal=await submit('mode-switch-fences-pending');
  const originalPromptCount=(await prompt()).approval_count;
  await nativeInput('child-mode-on');
  await poll(()=>findSession(sid),s=>s?.mode==='multi_child' && s.approval_mode==='automatic' &&
    s.available_capabilities.includes('child_open'),'parent advertises automatic mode');
  const invalidated=await poll(()=>taskStatus(oldNormal),s=>s.status==='cancelled','old normal-mode approval is invalidated');
  assert.equal(invalidated.output,'');
  assert.equal((await nativeInput('ping')).approval_open,false);

  const autoVi=await a.tool('start_interactive',{...openFile,request_id:'auto-vi-file'});
  assert.equal(autoVi.requires_local_command_approval,false);
  await poll(()=>taskStatus(autoVi),s=>s.status==='running','automatic mode starts vi');
  const viAction=(action,request_id,text)=>a.tool('request_task_action',{task_id:autoVi.task_id,action,request_id,text});
  await viAction('backspace','auto-vi-key');
  await poll(()=>taskStatus(autoVi),s=>s.key_request_id==='auto-vi-key' && s.key_status==='sent','automatic vi key');
  await viAction('insert_text','auto-vi-text','draft :qa!\n\t雪');
  await poll(()=>taskStatus(autoVi),s=>s.key_request_id==='auto-vi-text' && s.key_status==='sent','automatic vi text');
  await viAction('save','auto-vi-save');
  await poll(()=>taskStatus(autoVi),s=>s.key_request_id==='auto-vi-save' && s.key_status==='sent','automatic vi save');
  assert.equal((await nativeInput('ping')).file_save_count,1);
  await quit(autoVi);
  await poll(()=>taskStatus(autoVi),s=>s.channel_closed && s.quit_status==='sent','automatic vi exit');

  const createChild=async (request_id,client=a)=>{
    const open=await openChild(request_id,client);
    assert.equal(open.status,'queued');
    assert.equal(open.requires_local_command_approval,false);
    assert.equal((await openChild(request_id,client)).task_id,open.task_id);
    return poll(()=>client.tool('get_task_status',{task_id:open.task_id}),s=>s.status==='completed','automatic child registration');
  };
  await nativeInput('fail-next-child');
  const failedChild=await openChild('pty-refused');
  const childFailure=await poll(()=>taskStatus(failedChild),s=>s.status==='failed','PTY refusal is reported');
  assert.match(childFailure.error,/pty/i);
  const childA=await createChild('open-child-a');
  const childB=await createChild('open-child-b',b);
  const childAId=childA.child_session_id;
  const childBId=childB.child_session_id;
  assert.notEqual(childAId,childBId);
  assert.equal((await nativeInput('ping')).child_count,2);
  const parentWindow=await findSession(sid);
  const firstChildWindow=await findSession(childAId,b);
  assert.equal(firstChildWindow.mode,'child');
  assert.equal(firstChildWindow.approval_mode,'automatic');
  assert.equal(firstChildWindow.parent_session_id,sid);
  assert.equal(firstChildWindow.pid,parentWindow.pid);
  assert.notEqual(firstChildWindow.hwnd,parentWindow.hwnd);
  assert(firstChildWindow.available_capabilities.includes('child_close'));
  await assert.rejects(()=>openChild('nested',b,childAId),/parent|child|mode/i);
  const childWork=await b.tool('submit_command',{
    session_id:childAId,command:'synthetic-long-task',execution_timeout_ms:30000,request_id:'child-work',
  });
  assert.equal(childWork.requires_local_command_approval,false);
  const childStatus=()=>b.tool('get_task_status',{task_id:childWork.task_id});
  await poll(childStatus,s=>s.status==='running' && s.output.includes('SIMULATED child command result'),'long child command running');
  const otherChild=await attach('observe-child-b',childBId,a);
  await poll(()=>screen(otherChild),s=>s.frame?.text.includes('Child actions: 0'),'observe second child without input');
  await action(childWork,'child-space',space,b);
  await sent(childWork,'child-space',b);
  await poll(()=>screen(childWork,b),s=>s.frame?.text.includes('Child actions: 1'),'key reaches first child');
  assert((await screen(otherChild)).frame.text.includes('Child actions: 0'));
  await action(otherChild,'detach-child-b',{action:'detach'});
  await poll(()=>action(otherChild,'detach-child-b',{action:'detach'}),s=>s.action_status==='detached','release second child');
  await assert.rejects(()=>closeChild(childAId,'busy-close'),/busy|active|unresolved|idle/i);
  const parentWork=await submit('parent-while-child');
  assert.equal(parentWork.requires_local_command_approval,false);
  const parentResult=await poll(()=>taskStatus(parentWork),s=>s.status==='completed','parent command completes during child work');
  assert(parentResult.output.includes('SIMULATED command result'));
  assert(!parentResult.output.includes('SIMULATED child command result'));
  assert.equal((await childStatus()).status,'running');
  const readChild=await a.tool('read_terminal',{session_id:childAId,cursor:0,max_bytes:8192});
  assert(readChild.output.includes('synthetic-child-output'));
  assert(!readChild.output.includes('synthetic-native-output'));
  await b.tool('interrupt_task',{task_id:childWork.task_id});
  const stoppedChild=await poll(childStatus,s=>s.interrupt_status==='sent' && s.completion==='prompt_observed','automatic Ctrl+C returns to the child prompt');
  assert.equal(stoppedChild.exit_code,null);
  const childClose=await closeChild(childAId,'close-child-a');
  assert.equal(childClose.requires_local_command_approval,false);
  await poll(()=>taskStatus(childClose),s=>s.status==='completed','only target child closes');
  assert.equal(await findSession(childAId),undefined);
  assert(await findSession(childBId));
  assert(await findSession(sid));
  const modeSpanningTop=await a.tool('start_interactive',{
    session_id:sid,program:'top',request_id:'top-across-mode-switch',
  });
  await poll(()=>taskStatus(modeSpanningTop),s=>s.status==='running','automatic top starts before mode change');
  const autoFixture=await nativeInput('ping');
  assert.equal(autoFixture.approval_count,originalPromptCount,'no native approval window was created for any automatic action');
  assert.equal(autoFixture.approval_open,false);
  assert.equal(autoFixture.child_prompt_count,0);
  await nativeInput('child-mode-off');
  await poll(async()=>[await findSession(sid),await findSession(childBId)],
    ([parent,child])=>parent?.mode==='normal' && parent.approval_mode==='per_action' && !child,
    'switching mode off revokes AI on remaining child');
  assert.equal((await nativeInput('ping')).child_count,1);
  await assert.rejects(()=>openChild('mode-off'),/mode|child/i);
  await quit(modeSpanningTop);
  assert.equal((await prompt()).top_prompt,'quit','an already-running task uses current normal-mode approval for its new action');
  await nativeInput('allow');
  await poll(()=>taskStatus(modeSpanningTop),s=>s.channel_closed && s.quit_status==='sent','normal-mode approval exits the same top task');
  await nativeInput('hold');
  const restoredNormal=await submit('mode-off-normal-command');
  assert.equal(restoredNormal.requires_local_command_approval,true);
  assert.equal((await prompt()).approval_count,originalPromptCount+2);
  await nativeInput('allow');
  await poll(()=>taskStatus(restoredNormal),s=>s.status==='completed','normal approval works again');
  checks.push('Automatic mode isolates foreground input between child windows and runs supported commands, vi actions, interrupt and child lifecycle without dialogs');

  native.stdin.write('flood\n'); const flooded=await nextNative(); assert(flooded.dropped>0);
  await poll(()=>a.tool('read_terminal',{session_id:sid,cursor:0}),v=>v.lost_bytes>0,'native ring overflow');
  checks.push('native ring overflow discloses missing bytes without waiting for MCP');
  // Native event loop must answer while its IPC worker is offline.
  broker.kill(); await new Promise(r=>broker.once('exit',r));
  const pingStart=performance.now(); native.stdin.write('ping\n'); const ping=await nextNative(); const offlinePingMs=performance.now()-pingStart;
  assert(ping.ticks>state.ticks);
  broker=child(mcpPath,['--broker']); await sleep(100);
  await poll(()=>a.tool('list_sessions'),v=>v.sessions.some(s=>s.session_id===sid),'native and adapter recovery',10000);
  checks.push('broker killed: native GUI heartbeat survives; native and MCP reconnect without remote reconnection');
  await poll(()=>b.tool('list_sessions'),v=>v.sessions.some(s=>s.session_id===sid),'second adapter recovery');
  a.p.kill(); await sleep(100); const alive=await b.tool('read_terminal',{session_id:sid}); assert(alive.next_cursor>0);
  checks.push('MCP adapter crash does not interrupt another reader or native output');
  native.stdin.write('off\n'); await nextNative();
  await poll(()=>b.tool('list_sessions'),v=>!v.sessions.some(s=>s.session_id===sid),'disabled native unregister');
  await assert.rejects(()=>b.tool('read_terminal',{session_id:sid}));
  native.stdin.write('on\n'); const fresh=await nextNative(); assert.notEqual(fresh.session_hex,hex);
  await poll(()=>b.tool('list_sessions'),v=>v.sessions.length===1,'MCP-first registration');
  checks.push('Disable closes IPC and rejects the old identity; enabling after MCP starts registers a fresh session');
  const report={passed:true,checks,submit_ms:submitMs,native_event_loop_ping_ms_while_broker_offline:offlinePingMs,
    native_dropped_bytes:flooded.dropped,note:'synthetic frontend auto-approves synthetic commands; no SSH/bastion/real remote command used'};
  await writeFile(resolve(reportDir,'mcp-smoke-results.json'),JSON.stringify(report,null,2));
  console.log(JSON.stringify(report,null,2));
} finally {
  for(const p of children.reverse()) { if(p.exitCode===null) p.kill(); }
}
