// Disposable qualification workload. Never searches for or signals other apps.
// Parent/child control is through the IPC channel of an actually spawned child.
import {fork} from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';

const argumentsByName = Object.fromEntries(process.argv.slice(2).map((value,index,array)=>
  value.startsWith('--')?[value.slice(2),array[index+1]]:null).filter(Boolean));
const {role,nonce,directory,deadline:deadlineText} = argumentsByName;
const allocations = {root:8,child:16,grandchild:24,'grandchild-2':24};
const deadline = Number(deadlineText);
if (!(role in allocations) || !/^[0-9a-f]{32}$/.test(nonce??'') || !path.isAbsolute(directory??'') ||
    !Number.isSafeInteger(deadline) || deadline<=Date.now() || deadline>Date.now()+120000) {
  throw new Error('Invalid disposable helper contract');
}
const held = Buffer.alloc(allocations[role]*1024*1024,0x5a);
globalThis.qualificationHeldAllocation = held;
const own = {role,pid:process.pid,parentPid:process.ppid,allocationBytes:held.length,
  firstByte:held[0],lastByte:held[held.length-1],nonce};
const script = fileURLToPath(import.meta.url);
let descendant=null, stopping=false;
const timers=new Set();
const write = (name,value) => fs.writeFileSync(path.join(directory,name),JSON.stringify({...value,nonce})+'\n',{flag:'wx'});
const parentMessage = value => {if(process.connected)process.send({...value,nonce});};
const fail = async error => {
  if(role==='root') {try{write('fixture-error.json',{message:String(error?.message??error).slice(0,1000)});}catch{}}
  else parentMessage({kind:'error',message:String(error?.message??error).slice(0,1000)});
  await shutdown(2);
};

function spawnOwned(childRole) {
  const child=fork(script,['--role',childRole,'--nonce',nonce,'--directory',directory,'--deadline',String(deadline)],{
    windowsHide:true,stdio:['ignore','ignore','ignore','ipc'],
    env:{SystemRoot:process.env.SystemRoot,TEMP:process.env.TEMP,TMP:process.env.TMP},
  });
  let resolveReady,rejectReady,resolveExit;
  const ready=new Promise((resolve,reject)=>{resolveReady=resolve;rejectReady=reject;});
  const exited=new Promise(resolve=>{resolveExit=resolve;});
  child.once('error',rejectReady);
  child.once('exit',(code,signal)=>{rejectReady(new Error(`${childRole} exited before readiness`));resolveExit({code,signal,pid:child.pid});});
  child.on('message',message=>{
    if(message?.nonce!==nonce)return;
    if(message.kind==='ready') {
      if(!Array.isArray(message.members)||message.members[0]?.pid!==child.pid||message.members[0]?.role!==childRole||message.members[0]?.parentPid!==process.pid) {
        rejectReady(new Error('Owned child handshake mismatch'));return;
      }
      resolveReady(message.members);
    } else if(message.kind==='error') {void fail(new Error(message.message));}
    else if(role==='root'&&message.kind==='grandchild-exited')write('grandchild-exited.json',{...message});
    else if(role==='root'&&message.kind==='replacement-ready')write('replacement-ready.json',{...message});
  });
  return {child,ready,exited,role:childRole};
}

async function stopOwned(owned) {
  if(!owned)return;
  if(owned.child.connected)owned.child.send({command:'shutdown',nonce});
  let timer;
  const result=await Promise.race([owned.exited,new Promise(resolve=>{timer=setTimeout(()=>resolve(null),2000);})]);
  clearTimeout(timer);
  // No PID signal fallback. Parent exit closes the owned IPC channel; each
  // descendant handles disconnect and has its own shared finite deadline.
  if(!result)throw new Error('Owned child did not acknowledge exit before helper deadline');
  return result;
}

async function shutdown(code=0) {
  if(stopping)return;
  stopping=true;
  for(const timer of timers)clearInterval(timer);
  try{await stopOwned(descendant);}catch{code=2;}
  process.exit(code); // self only, never another PID.
}

process.on('disconnect',()=>{void shutdown();});
process.on('uncaughtException',error=>{void fail(error);});
process.on('unhandledRejection',error=>{void fail(error);});
process.on('message',message=>{
  if(message?.nonce!==nonce||stopping)return;
  if(message.command==='shutdown'){void shutdown();return;}
  if(role!=='child')return;
  if(message.command==='stop-grandchild') {
    void(async()=>{const old=descendant;if(!old)throw new Error('Missing original grandchild');
      const result=await stopOwned(old);descendant=null;parentMessage({kind:'grandchild-exited',...result});})().catch(fail);
  } else if(message.command==='replace-grandchild') {
    void(async()=>{if(descendant)throw new Error('Prior grandchild still retained');
      descendant=spawnOwned('grandchild-2');parentMessage({kind:'replacement-ready',members:await descendant.ready});})().catch(fail);
  }
});
const selfDeadline=setTimeout(()=>{void shutdown(2);},Math.max(1,deadline-Date.now()));
timers.add(selfDeadline);

if(role==='root'||role==='child')descendant=spawnOwned(role==='root'?'child':'grandchild');
const members=[own,...(descendant?await descendant.ready:[])];
if(role!=='root')parentMessage({kind:'ready',members});
else {
  write('tree-ready.json',{kind:'ready',members});
  const handled=new Set();
  const control=setInterval(()=>{
    for(const [file,command] of [['stop-grandchild.json','stop-grandchild'],['replace-grandchild.json','replace-grandchild'],['shutdown.json','shutdown']]) {
      if(handled.has(file))continue;
      const location=path.join(directory,file);
      if(!fs.existsSync(location))continue;
      if(fs.statSync(location).size>4096){void fail(new Error('Oversize fixture command'));return;}
      let value;try{value=JSON.parse(fs.readFileSync(location,'utf8'));}catch{continue;}
      if(value.nonce!==nonce){void fail(new Error('Fixture command nonce mismatch'));return;}
      handled.add(file);
      if(command==='shutdown'){void shutdown();return;}
      if(!descendant?.child.connected){void fail(new Error('Owned child IPC disconnected'));return;}
      descendant.child.send({command,nonce});
    }
    // Read the held data repeatedly so the deliberate allocation remains used.
    if(held[0]!==0x5a||held[held.length-1]!==0x5a)void fail(new Error('Held allocation changed'));
  },50);
  timers.add(control);
}
