import test from 'node:test';
import assert from 'node:assert/strict';
import {interpretTraceFiles} from './trace-evidence.mjs';
const json=JSON.stringify;
const jsonl=rows=>rows.map(json).join('\n')+'\n';
function files(){
 const identity={nonce:'a'.repeat(32),controllerPid:10,controllerCreated:'10',sessionGuid:'0fe184ed-aa99-40a2-86fe-9b2a5c9063a2'};
 return {
  'trace-ready.json':json({...identity,readyFileTime:'100000000',sessionHandle:'5',sessionName:'ImageRankerQualificationProcess',etl:'C:\\owned\\process.etl'}),
  'trace-root.json':json({...identity,pid:100,created:'110000000',boundFileTime:'120000000',verified:true}),
  'trace-terminal.json':json({...identity,stopConfirmed:true,decodeComplete:true,reason:'cooperative-stop',eventsLost:0,logBuffersLost:0,realTimeBuffersLost:0,consumerLost:0,decodeErrors:0,issues:[]}),
  'trace-statistics.jsonl':jsonl([{sequence:0,statistics:{Handle:'5',Guid:identity.sessionGuid,Name:'ImageRankerQualificationProcess',Path:'C:\\owned\\process.etl',EventsLost:0,LogBuffersLost:0,RealTimeBuffersLost:0}}]),
  'trace-events.jsonl':jsonl([{type:'start',pid:100,parentPid:50,created:'110000000',at:'110000000'},{type:'stop',pid:100,created:'110000000',exit:'130000000',at:'130000000'},{type:'terminal-counter',pid:100,at:'130000000',peakWorkingSetBytes:'4096'}]),
 };
}
test('complete native-shaped receipts remain model evidence, not executed trace qualification',()=>{
 const r=interpretTraceFiles(files());assert.equal(r.qualification,'model-evidence-complete');assert.equal(r.nativeCaptureQualified,false);assert.equal(r.artifacts.length,5);
});
test('nonce/controller/session swaps between readiness, root and terminal are rejected',()=>{
 for(const name of ['trace-root.json','trace-terminal.json'])for(const field of ['nonce','controllerPid','controllerCreated','sessionGuid']){
  const f=files(),changed=JSON.parse(f[name]);changed[field]='foreign';f[name]=json(changed);assert.throws(()=>interpretTraceFiles(f),/differs|another capture/);
 }
});
test('earlier real loss cannot be erased by a later zero terminal snapshot',()=>{
 const f=files(),stat=JSON.parse(f['trace-statistics.jsonl']);stat.statistics.EventsLost=1;f['trace-statistics.jsonl']=jsonl([stat]);
 assert.ok(interpretTraceFiles(f).issues.includes('nonzero-eventsLost'));
});
test('statistics from foreign handle/GUID/path or missing sequence are rejected',()=>{
 for(const key of ['Handle','Guid','Name','Path']){
  const f=files(),stat=JSON.parse(f['trace-statistics.jsonl']);stat.statistics[key]='foreign';f['trace-statistics.jsonl']=jsonl([stat]);assert.throws(()=>interpretTraceFiles(f),/different native session/);
 }
 const f=files(),stat=JSON.parse(f['trace-statistics.jsonl']);stat.sequence=1;f['trace-statistics.jsonl']=jsonl([stat]);assert.throws(()=>interpretTraceFiles(f),/Missing or reordered/);
});
test('torn output is never trimmed into a passing trace',()=>{
 const f=files();f['trace-events.jsonl']=f['trace-events.jsonl'].trimEnd();assert.throws(()=>interpretTraceFiles(f),/torn/);
});
test('GPU rows without a matching successful terminal receipt stay independently incomplete',()=>{
 const f=files();f['gpu-samples.jsonl']=jsonl([{begin:'120000000',end:'120000001',values:[],error:'unavailable'}]);
 const r=interpretTraceFiles(f);assert.equal(r.gpu.captureComplete,false);assert.equal(r.gpu.measurement,'unavailable');
});
