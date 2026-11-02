import {spawn} from 'node:child_process';
import {createServer} from 'node:net';
import {writeFile} from 'node:fs/promises';
import {dirname,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
const root=resolve(dirname(fileURLToPath(import.meta.url)),'..');
const outcomes=[];
for(const [label,exe] of [
  ['original_0_85',resolve(root,'build-baseline/Release/putty.exe')],
  ['modified_runtime_AI_OFF',resolve(root,'build-ai/Release/putty.exe')],
]) {
  let bytes=0; const sockets=new Set();
  const block=Buffer.from(('local synthetic benchmark line 0123456789 abcdefghijklmnopqrstuvwxyz\r\n').repeat(240));
  const server=createServer(socket=>{
    sockets.add(socket); socket.on('error',()=>{});
    const timer=setInterval(()=>{if(socket.writable && !socket.writableNeedDrain){socket.write(block);bytes+=block.length;}},10);
    socket.on('close',()=>{clearInterval(timer);sockets.delete(socket);});
  });
  await new Promise(r=>server.listen(0,'127.0.0.1',r));
  const child=spawn(exe,['-raw','-P',String(server.address().port),'127.0.0.1'],{windowsHide:true,stdio:'ignore'});
  try {
    const start=performance.now();
    const monitor=spawn('powershell.exe',['-NoProfile','-ExecutionPolicy','Bypass','-File',resolve(root,'tests/measure-window.ps1'),'-ProcessId',String(child.pid)],{windowsHide:true,stdio:['ignore','pipe','pipe']});
    let stdout='',stderr=''; monitor.stdout.on('data',b=>stdout+=b); monitor.stderr.on('data',b=>stderr+=b);
    const code=await new Promise(r=>monitor.once('exit',r)); if(code) throw Error(stderr);
    outcomes.push({label,...JSON.parse(stdout),offered_tcp_bytes:bytes,offered_tcp_MiB_per_sec:bytes/(1024*1024)/((performance.now()-start)/1000)});
  } finally {
    child.kill(); for(const s of sockets)s.destroy(); await new Promise(r=>server.close(r));
  }
}
const report={kind:'real_hidden_PuTTY_windows_local_raw_backend',outcomes,
  scope:'Loopback synthetic text; 100 WM_NULL probes per actual PuTTY GUI. AI is OFF in both GUI runs. Not an enabled-AI SSH/bastion throughput measurement. Offered TCP bytes do not prove rendering throughput.'};
await writeFile(resolve(root,'docs/ui-benchmark-results.json'),JSON.stringify(report,null,2)); console.log(JSON.stringify(report,null,2));
