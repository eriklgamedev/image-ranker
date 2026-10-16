import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {summarizeTrace} from './trace-ledger.mjs';

const limits={
 'trace-ready.json':65536,'trace-root.json':65536,'trace-terminal.json':65536,
 'trace-events.jsonl':64*1024*1024,'trace-statistics.jsonl':2*1024*1024,
 'gpu-samples.jsonl':16*1024*1024,'gpu-summary.json':65536,
};
function json(text,name){
 if(typeof text!=='string'||Buffer.byteLength(text)>limits[name])throw new Error(`Missing or oversized ${name}`);
 return JSON.parse(text);
}
function lines(text,name,maximum){
 if(typeof text!=='string'||Buffer.byteLength(text)>limits[name]||!text.endsWith('\n'))throw new Error(`Missing, oversized or torn ${name}`);
 const rows=text.slice(0,-1).split('\n');
 if(rows.length>maximum)throw new Error(`Too many records in ${name}`);
 return rows.filter(Boolean).map(line=>{if(Buffer.byteLength(line)>1_048_576)throw new Error(`Oversized record in ${name}`);return JSON.parse(line)});
}
export function interpretTraceFiles(files){
 const ready=json(files['trace-ready.json'],'trace-ready.json');
 const root=json(files['trace-root.json'],'trace-root.json');
 const terminal=json(files['trace-terminal.json'],'trace-terminal.json');
 for(const key of ['nonce','controllerPid','controllerCreated','sessionGuid']){
  if(ready[key]===undefined||ready[key]!==terminal[key])throw new Error(`Terminal ${key} differs from the ready receipt`);
 }
 if(root.nonce!==ready.nonce||root.controllerPid!==ready.controllerPid||root.controllerCreated!==ready.controllerCreated||root.sessionGuid!==ready.sessionGuid)throw new Error('Root binding belongs to another capture');
 const events=lines(files['trace-events.jsonl'],'trace-events.jsonl',250000);
 const stats=lines(files['trace-statistics.jsonl'],'trace-statistics.jsonl',3602);
 if(!stats.length)throw new Error('Missing trace loss observations');
 let previous=-1;
 const cumulative={eventsLost:0,logBuffersLost:0,realTimeBuffersLost:0};
 for(const stat of stats){
  if(stat.sequence!==previous+1)throw new Error('Missing or reordered trace statistics');previous=stat.sequence;
  const native=stat.statistics;
  if(native.Guid!==ready.sessionGuid||native.Handle!==ready.sessionHandle||native.Name!==ready.sessionName||native.Path!==ready.etl)throw new Error('Statistics belong to a different native session');
  for(const key of Object.keys(cumulative)){
   const nativeKey=key[0].toUpperCase()+key.slice(1),value=native[nativeKey];
   if(!Number.isSafeInteger(value)||value<0)throw new Error('Invalid native loss statistic');
   cumulative[key]=Math.max(cumulative[key],value);
  }
 }
 // A later zero cannot erase an earlier actual loss observation.
 for(const [key,value] of Object.entries(cumulative))if(Number.isSafeInteger(terminal[key]))terminal[key]=Math.max(terminal[key],value);
 const gpu=files['gpu-samples.jsonl']===undefined?[]:lines(files['gpu-samples.jsonl'],'gpu-samples.jsonl',3600);
 const summary=summarizeTrace({schema:1,readyFileTime:ready.readyFileTime,root,terminal,events,gpu});
 if(gpu.length){
  const end=files['gpu-summary.json']===undefined?null:json(files['gpu-summary.json'],'gpu-summary.json');
  if(!end||end.samples!==gpu.length||end.reason!=='controller-finished'||end.error)summary.gpu.captureComplete=false;
  else summary.gpu.captureComplete=true;
 }else summary.gpu.captureComplete=false;
 summary.artifacts=Object.entries(files).map(([name,text])=>({name,sha256:createHash('sha256').update(text).digest('hex')}));
 return summary;
}

function readBounded(directory,name){
 const filename=path.join(directory,name);
 if(!fs.existsSync(filename))return undefined;
 const fd=fs.openSync(filename,'r');
 try {
  const before=fs.fstatSync(fd);if(!before.isFile()||before.size>limits[name])throw new Error(`Invalid bound/type for ${name}`);
  const buffer=Buffer.alloc(before.size);let offset=0;
  while(offset<buffer.length){const n=fs.readSync(fd,buffer,offset,buffer.length-offset,null);if(n===0)throw new Error(`Short read ${name}`);offset+=n;}
  const after=fs.fstatSync(fd);if(after.size!==before.size||after.mtimeMs!==before.mtimeMs)throw new Error(`Evidence changed while reading ${name}`);
  return new TextDecoder('utf-8',{fatal:true}).decode(buffer);
 }finally{fs.closeSync(fd);}
}
if(process.argv[1]&&path.resolve(process.argv[1])===fileURLToPath(import.meta.url)){
 if(process.argv.length!==3)throw new Error('Usage: node trace-evidence.mjs <owned completed trace directory>');
 const directory=path.resolve(process.argv[2]),files={};
 for(const name of Object.keys(limits)){const text=readBounded(directory,name);if(text!==undefined)files[name]=text;}
 const result=interpretTraceFiles(files);
 fs.writeFileSync(path.join(directory,'trace-summary.json'),JSON.stringify(result,null,2)+'\n',{flag:'wx'});
 console.log(JSON.stringify({qualification:result.qualification,processCount:result.processCount,nativeCaptureQualified:false,gpu:result.gpu.measurement}));
}
