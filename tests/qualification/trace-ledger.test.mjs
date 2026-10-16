import test from 'node:test';
import assert from 'node:assert/strict';
import {summarizeTrace,traceIdentity} from './trace-ledger.mjs';

const time = n=>String(1_000_000_000n+BigInt(n)*10_000_000n);
const start=(pid,parentPid,n)=>({type:'start',pid,parentPid,created:time(n),at:time(n)});
const stop=(pid,created,n)=>({type:'stop',pid,created:time(created),exit:time(n),at:time(n)});
const peak=(pid,n,bytes)=>({type:'terminal-counter',pid,at:time(n),peakWorkingSetBytes:String(bytes)});
function fixture(){return {
 schema:1,readyFileTime:time(0),root:{pid:100,created:time(1),boundFileTime:time(2),verified:true},
 events:[start(100,50,1),start(101,100,3),stop(101,3,4),peak(101,4,600),stop(100,1,8),peak(100,8,400)],
 terminal:{stopConfirmed:true,decodeComplete:true,reason:'cooperative-stop',eventsLost:0,logBuffersLost:0,realTimeBuffersLost:0,consumerLost:0,decodeErrors:0,issues:[]},gpu:[],
};}
const identity=(pid,n)=>traceIdentity(pid,time(n));
test('loss-free prelaunch evidence retains short child terminal peak without inventing tree maximum',()=>{
 const result=summarizeTrace(fixture());assert.equal(result.qualification,'model-evidence-complete');assert.equal(result.processCount,2);
 assert.equal(result.processes.find(p=>p.identity===identity(101,3)).terminalPeakWorkingSetBytes,'600');
 assert.equal(result.combinedLifetimeWorkingSetPeakBytes,null);assert.equal(result.nativeCaptureQualified,false);assert.equal(result.gpu.measurement,'unavailable');
});
test('arrival order does not replace native parent/creation/time evidence',()=>{
 const f=fixture();f.events.reverse();assert.equal(summarizeTrace(f).qualification,'model-evidence-complete');
});
test('preexisting unrelated system process stop does not fabricate a measured member',()=>{
 const f=fixture();f.events.push(stop(500,-10,4),peak(500,4,2000));
 const r=summarizeTrace(f);assert.equal(r.qualification,'model-evidence-complete');assert.equal(r.processCount,2);
});
test('retained child stays in tree after actual parent exit',()=>{
 const f=fixture();f.events=[start(100,50,1),start(101,100,3),stop(100,1,4),peak(100,4,400),start(102,101,5),stop(102,5,6),peak(102,6,200),stop(101,3,8),peak(101,8,600)];
 assert.equal(summarizeTrace(f).processCount,3);assert.equal(summarizeTrace(f).qualification,'model-evidence-complete');
});
test('reused parent PID is never attributed to its earlier tree identity',()=>{
 const f=fixture();f.events.push(start(100,50,10),start(200,100,11),stop(200,11,12),peak(200,12,999999),stop(100,10,14),peak(100,14,888888));
 const result=summarizeTrace(f);assert.equal(result.processCount,2);assert.ok(!result.processes.some(p=>p.identity===identity(200,11)));
});
test('overlapping PID lifetimes cannot qualify identity coverage',()=>{
 const f=fixture();f.events.push(start(101,100,3),start(101,100,4));
 assert.ok(summarizeTrace(f).issues.includes('overlapping-reused-pid-lifetimes'));
});
test('missing root start, unverified binding or postlaunch readiness blocks qualification',()=>{
 for(const change of [f=>f.events.shift(),f=>f.root.verified=false,f=>f.readyFileTime=time(2)]){
  const f=fixture();change(f);assert.equal(summarizeTrace(f).qualification,'incomplete');
 }
});
test('parent which had exited before creation does not grant descendant membership',()=>{
 const f=fixture();f.events.push(start(103,101,7),stop(103,7,9),peak(103,9,999));
 assert.ok(summarizeTrace(f).issues.includes('unresolved-descendant-parent-lifetime'));
});
test('native stop and exact terminal peak are required, not inferred from last sample',()=>{
 for(const type of ['stop','terminal-counter']){
  const f=fixture();f.events=f.events.filter(e=>!(e.type===type&&e.pid===101));
  const result=summarizeTrace(f);assert.equal(result.qualification,'incomplete');
  if(type==='terminal-counter')assert.equal(result.processes.find(p=>p.identity===identity(101,3)).terminalPeakWorkingSetBytes,null);
 }
});
test('ambiguous terminal PID after rapid reuse fails instead of selecting nearest process',()=>{
 const f=fixture();f.events.push(start(101,100,5),stop(101,5,6),peak(101,5,1234),peak(101,6,1234));
 assert.ok(summarizeTrace(f).issues.includes('ambiguous-or-unmatched-terminal-counter'));
});
test('every native loss channel and terminal decode failure blocks qualification',()=>{
 for(const key of ['eventsLost','logBuffersLost','realTimeBuffersLost','consumerLost','decodeErrors']){
  const f=fixture();f.terminal[key]=1;assert.ok(summarizeTrace(f).issues.includes(`nonzero-${key}`));
  delete f.terminal[key];assert.ok(summarizeTrace(f).issues.includes(`missing-${key}`));
 }
});
test('unknown TDH schema and bounded capture exhaustion are retained as failure evidence',()=>{
 const f=fixture();f.events.push({type:'issue',at:time(6),reason:'unsupported-process-counter-schema'});f.terminal.reason='output-limit';
 const r=summarizeTrace(f);assert.ok(r.issues.includes('unsupported-process-counter-schema'));assert.equal(r.qualification,'incomplete');
});
test('conflicting starts/stops/counters cannot silently win by arrival order',()=>{
 for(const event of [{...start(101,100,3),parentPid:99},stop(101,3,7),peak(101,4,601)]){
  const f=fixture();f.events.push(event);assert.equal(summarizeTrace(f).qualification,'incomplete');
 }
});
const gpuValue=(override={})=>({pid:101,created:time(3),adapter:'luid_0x00000000_0x00000042_phys_0',counter:'dedicated',bytes:'250',status:0,heldBefore:true,aliveAfter:true,...override});
test('GPU values bind exact native creation and adapter, never PID alone or a lifetime maximum',()=>{
 const f=fixture();f.gpu=[{begin:time(3),end:String(BigInt(time(3))+100n),values:[gpuValue(),gpuValue({created:time(2),counter:'shared'})]}];
 const r=summarizeTrace(f);assert.equal(r.gpu.acceptedValues,1);assert.equal(r.gpu.rejectedValues,1);assert.equal(r.gpu.lifetimePeakBytes,null);assert.equal(r.gpu.measurement,'sampled-only');
});
test('missing pre-held identity, counter error, and exit during collection are unknown not zero',()=>{
 for(const value of [gpuValue({heldBefore:false}),gpuValue({status:0xc0000bc6}),gpuValue({aliveAfter:false})]){
  const f=fixture();f.gpu=[{begin:time(3),end:time(4),values:[value]}];const r=summarizeTrace(f);
  assert.equal(r.gpu.measurement,'unavailable');assert.equal(r.gpu.perIdentitySampledPeaks.length,0);assert.equal(r.gpu.rejectedValues,1);
 }
});
test('valid new PDH gauge data is accepted but unresolved native identities remain rejected',()=>{
 const f=fixture();f.gpu=[{begin:time(3),end:time(3),values:[gpuValue({status:1})],unresolved:[{pid:200,reason:'native-process-identity-unavailable'}]}];
 const r=summarizeTrace(f);assert.equal(r.gpu.acceptedValues,1);assert.equal(r.gpu.rejectedValues,1);
});
test('GPU collection is limited to one Hz, non-overlapping and distinct counter rows',()=>{
 const f=fixture();const sample={begin:time(3),end:time(3),values:[gpuValue()]};f.gpu=[sample,{...sample,begin:String(BigInt(time(3))+1n),end:time(4)}];
 assert.throws(()=>summarizeTrace(f),/1 Hz/);f.gpu=[{...sample,values:[gpuValue(),gpuValue()]}];assert.throws(()=>summarizeTrace(f),/Duplicate GPU/);
});
test('unsafe numbers, oversized input and unknown normalized records reject rather than coerce',()=>{
 assert.throws(()=>traceIdentity(1,1),/creation/);assert.throws(()=>traceIdentity(1,'18446744073709551616'),/Overflow/);
 const f=fixture();f.events.push({type:'sample',at:time(6)});assert.throws(()=>summarizeTrace(f),/Unknown/);
 assert.throws(()=>summarizeTrace({...fixture(),events:Array(250001)}),/bounded/);
});
