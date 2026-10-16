import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {validateSchema,mergePatch,UnsupportedSchema} from './schema.mjs';
import {validatePackageConfig} from './validate-config.mjs';
import {processKey,mayAdmitChild,summarizeMemory} from './process-model.mjs';
import {measureLatency,summarizeLatency,writeLatencyResult} from './latency.mjs';
import {readMemoryCapture} from './summarize-memory.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)),'../..');
function removeOwnedFixture(directory) {
  const resolved=path.resolve(directory);
  assert.equal(path.dirname(resolved),path.resolve(os.tmpdir()));
  assert.ok(path.basename(resolved).startsWith('image-ranker-qualification-unit-'));
  assert.equal(fs.realpathSync(resolved),resolved);
  fs.rmSync(resolved,{recursive:true,force:true});
}
const header = {type:'header',schema:1,rootPid:10,rootCreated:'100',coverage:'attach-polling-only',intervalMs:100};
const processRow = (pid,created,parentPid=null,parentCreated=null,working=10,privateBytes=20,peak=100) =>
  ({pid,created,parentPid,parentCreated,status:'sampled',workingSetBytes:working,privateCommitBytes:privateBytes,lifetimePeakWorkingSetBytes:peak});
const sample = (sequence,processes,issues=[]) => ({type:'sample',sequence,startMs:sequence*100,endMs:sequence*100+5,processes,issues});
const terminal = samples => ({schema:1,samples,coverage:'attach-polling-only',stopReason:'observed-members-exited',issues:[],survivingObservedProcesses:[]});
const rootProcess = ()=>processRow(10,'100');
const child = ()=>processRow(11,'200',10,'100',30,40,500);
const latencyOptions = overrides => ({action:async({actionId})=>({actionId}),
  verify:(value,{actionId})=>value.actionId===actionId,metric:'command-ack',
  boundaryDefinition:'fixture adapter invocation to exact result resolution',buildKind:'unit-fixture',
  count:3,warmup:0,minimumSamples:3,thresholdMs:1000,actionTimeoutMs:100,drainTimeoutMs:10,maxRunMs:1000,...overrides});

test('qualification overlay merges and validates against pinned local Tauri schema',()=>{
  const result=validatePackageConfig(root);
  assert.equal(result.validation,'passed');
  assert.equal(result.merged.bundle.windows.nsis.installMode,'currentUser');
  assert.equal(result.merged.bundle.windows.webviewInstallMode.type,'offlineInstaller');
  assert.equal(result.merged.bundle.windows.allowDowngrades,false);
  assert.equal(result.qualification,'not-built-or-installed');
});

test('schema validator rejects unknown fields, wrong types, unsupported keywords and remote references',()=>{
  const schema={type:'object',properties:{enabled:{type:'boolean'}},required:['enabled'],additionalProperties:false};
  assert.deepEqual(validateSchema({enabled:false},schema),[]);
  assert.ok(validateSchema({enabled:'false'},schema).length);
  assert.ok(validateSchema({enabled:true,other:1},schema).length);
  assert.throws(()=>validateSchema({}, {unevaluatedProperties:false}),UnsupportedSchema);
  assert.throws(()=>validateSchema({}, {$ref:'https://schema.invalid/remote'}),/disabled/);
  assert.throws(()=>validateSchema({}, {anyOf:[{}, {unknownKeyword:true}]}),/Unsupported/);
});

test('schema combination/ref/tuple/dependency rules do not silently pass invalid input',()=>{
  const schema={definitions:{positive:{type:'integer',minimum:1}},type:'object',
    properties:{n:{$ref:'#/definitions/positive'},pair:{type:'array',items:[{type:'string'},{type:'boolean'}],additionalItems:false}},
    dependencies:{n:['pair']},required:['n']};
  assert.deepEqual(validateSchema({n:1,pair:['a',false]},schema),[]);
  for(const input of [{n:0,pair:['a',false]},{n:1},{n:1,pair:['a',false,7]}]) assert.ok(validateSchema(input,schema).length);
  assert.ok(validateSchema(1,{oneOf:[{type:'integer'},{type:'number'}]}).length);
  assert.ok(validateSchema('a',{not:{const:'a'}}).length);
});

test('config merge replaces arrays and removes null keys without prototype injection',()=>{
  const base={build:{features:['e2e'],x:1},targets:['all']};
  assert.deepEqual(mergePatch(base,{build:{features:[],x:null},targets:['nsis']}),{build:{features:[]},targets:['nsis']});
  assert.deepEqual(base,{build:{features:['e2e'],x:1},targets:['all']});
  assert.throws(()=>mergePatch({},JSON.parse('{"__proto__":{"polluted":true}}')),/Unsafe/);
});

test('process identity retains full filetime precision and child admission rejects reused/ended parents',()=>{
  assert.notEqual(processKey(10,'134000000000000001'),processKey(10,'134000000000000002'));
  assert.ok(mayAdmitChild(rootProcess(),child()));
  assert.equal(mayAdmitChild({...rootProcess(),status:'exited'},child()),false);
  assert.equal(mayAdmitChild({...rootProcess(),created:'300'},child()),false);
  assert.equal(mayAdmitChild(rootProcess(),{...child(),parentCreated:'99'}),false);
  assert.throws(()=>processKey(10,100),/Invalid/);
});

test('memory sums observed counters separately and never calls CPU working set GPU/heap/unique RAM',()=>{
  const records=[header,sample(0,[child(),rootProcess()]),sample(1,[{...rootProcess(),status:'exited'},{...child(),workingSetBytes:35}])];
  const result=summarizeMemory(records,terminal(2));
  assert.equal(result.sampledSumWorkingSetPeakBytes,40);
  assert.equal(result.sampledSumPrivateCommitPeakBytes,60);
  assert.equal(result.perProcessLifetimeWorkingSetPeaks.length,2);
  assert.equal(result.gpuMeasurement,'unavailable');
  assert.equal(result.peakQualification,'not-established');
  assert.match(result.labels.workingSet,/shared pages/);
  assert.match(result.labels.privateCommit,/not heap/);
  assert.ok(!Object.hasOwn(result,'totalMemoryBytes'));
});

test('unrelated same-name process and wrong parent lifetime are rejected before totals',()=>{
  const unrelated={...processRow(50,'500',999,'1'),imageName:'image-ranker.exe'};
  assert.throws(()=>summarizeMemory([header,sample(0,[rootProcess(),unrelated])],terminal(1)),/Unproved/);
  assert.throws(()=>summarizeMemory([header,sample(0,[rootProcess(),{...child(),parentCreated:'1'}])],terminal(1)),/Unproved/);
});

test('known child remains observed after parent exit; reused PID is a new exact identity',()=>{
  const records=[header,sample(0,[rootProcess(),child()]),
    sample(1,[rootProcess(),{...child(),status:'exited'}]),
    sample(2,[rootProcess(),{...child(),created:'400'}]),
    sample(3,[{...rootProcess(),status:'exited'},{...child(),created:'400'}])];
  const result=summarizeMemory(records,terminal(4));
  assert.equal(result.processIdentities,3);
  assert.equal(result.completeObservedSamples,4);
  assert.throws(()=>summarizeMemory([header,...records.slice(1,3),sample(2,[rootProcess(),child()])],terminal(3)),/reappeared/);
});

test('missing and denied samples never count as zero or establish complete peaks',()=>{
  const records=[header,sample(0,[rootProcess(),child()]),sample(1,[{...rootProcess(),status:'query-failed'}])];
  const result=summarizeMemory(records,null);
  assert.equal(result.completeObservedSamples,1);
  assert.equal(result.sampledSumWorkingSetPeakBytes,40);
  assert.ok(result.issues.includes('known-live-process-missing'));
  assert.ok(result.issues.includes('member-query-failed'));
  assert.ok(result.issues.includes('missing-or-inconsistent-terminal-record'));
  const none=summarizeMemory([header,sample(0,[{...rootProcess(),status:'query-failed'}])],terminal(1));
  assert.equal(none.sampledSumWorkingSetPeakBytes,null);
});

test('out of order, duplicate, unsafe or negative memory records are rejected',()=>{
  for(const records of [[header,sample(1,[rootProcess()])],[header,sample(0,[rootProcess(),rootProcess()])],
    [header,sample(0,[{...rootProcess(),workingSetBytes:-1}])],
    [header,sample(0,[{...rootProcess(),privateCommitBytes:Number.MAX_SAFE_INTEGER+1}])]]) {
    assert.throws(()=>summarizeMemory(records,terminal(1)));
  }
});

test('memory capture reader preserves a torn suffix rather than silently repairing',()=>{
  const directory=fs.mkdtempSync(path.join(os.tmpdir(),'image-ranker-qualification-unit-'));
  try{
    const log=path.join(directory,'memory.jsonl');
    const bytes=JSON.stringify(header)+'\n{"type":"sample"';fs.writeFileSync(log,bytes);
    assert.throws(()=>readMemoryCapture(directory),/Incomplete/);
    assert.equal(fs.readFileSync(log,'utf8'),bytes);
  }finally{removeOwnedFixture(directory);}
});

test('latency percentiles retain failed attempts and cannot certify the release',()=>{
  const samples=Array.from({length:20},(_,index)=>({phase:'measure',outcome:'success',elapsedMs:index+1}));
  const options={thresholdMs:20,minimumSamples:20,buildKind:'debug-e2e',metric:'command-ack',boundaryDefinition:'invoke to command acknowledgment'};
  assert.equal(summarizeLatency(samples,options).p95SuccessfulOnlyMs,19);
  assert.equal(summarizeLatency(samples,options).observedThreshold,'met');
  const withFailure=summarizeLatency([...samples,{phase:'measure',outcome:'timeout',elapsedMs:5000}],options);
  assert.equal(withFailure.observedThreshold,'incomplete');
  assert.equal(withFailure.failedAttempts,1);
  assert.equal(withFailure.releaseQualification,'not-established-by-this-summary');
});

test('actual adapter results require exact semantic verification',async()=>{
  const good=await measureLatency(latencyOptions({}));
  assert.equal(good.summary.observedThreshold,'met');
  const rejected=await measureLatency(latencyOptions({action:async()=>({actionId:'stale'})}));
  assert.equal(rejected.samples[0].outcome,'rejected');
  assert.equal(rejected.samples.length,1);
  assert.equal(rejected.summary.observedThreshold,'incomplete');
});

test('async verifier is rejected without an unhandled rejection or next action',async()=>{
  const result=await measureLatency(latencyOptions({verify:async()=>{throw new Error('late invalid proof');}}));
  assert.equal(result.samples[0].outcome,'rejected');
  assert.match(result.samples[0].reason,/synchronous/);
  assert.equal(result.samples.length,1);
  assert.equal(result.summary.observedThreshold,'incomplete');
});

test('timeout preserves unsettled action ownership and prevents a second admission',async()=>{
  let calls=0,settle;
  const result=await measureLatency(latencyOptions({action:()=>{calls++;return new Promise(resolve=>{settle=resolve;});},actionTimeoutMs:2,drainTimeoutMs:2}));
  assert.equal(calls,1);
  assert.equal(result.stopReason,'action-ownership-unresolved');
  assert.ok(result.pendingAction instanceof Promise);
  assert.equal(result.summary.observedThreshold,'incomplete');
  settle({actionId:result.samples[0].actionId});
  assert.equal((await result.pendingAction).kind,'settled');
});

test('cancel ACK and action settlement remain distinct, with bounded drain',async()=>{
  const control=new AbortController();
  const result=await measureLatency(latencyOptions({signal:control.signal,action:({signal})=>new Promise(resolve=>{
    signal.addEventListener('abort',()=>resolve({cancelled:true}),{once:true});
    control.abort();
  })}));
  assert.equal(result.samples[0].outcome,'cancelled');
  assert.equal(result.samples[0].settledAfterAbort,true);
  assert.equal(result.pendingAction,null);
  assert.equal(result.summary.observedThreshold,'incomplete');
});

test('evidence publication refuses overwrite and serializes unresolved ownership honestly',()=>{
  const directory=fs.mkdtempSync(path.join(os.tmpdir(),'image-ranker-qualification-unit-'));
  try{
    const file=path.join(directory,'latency.json');
    const result={runId:'test',stopReason:'action-ownership-unresolved',summary:{},samples:[],pendingAction:Promise.resolve()};
    writeLatencyResult(file,result);
    assert.equal(JSON.parse(fs.readFileSync(file)).unresolvedAction,true);
    const before=fs.readFileSync(file);
    assert.throws(()=>writeLatencyResult(file,result),/EEXIST/);
    assert.deepEqual(fs.readFileSync(file),before);
  }finally{removeOwnedFixture(directory);}
});
