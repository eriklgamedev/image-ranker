import test from 'node:test';
import assert from 'node:assert/strict';
import {checkpointExpectation,checkpointMetadata,verifyCheckpointChange,checkpointSubsets} from './decision-checkpoints.mjs';
import {summarizeLatency} from './latency.mjs';
const metadata=revision=>checkpointMetadata(Buffer.from(JSON.stringify({schemaVersion:2,revision,
  sessionId:'12345678-1234-1234-1234-123456789abc',logGeneration:'abcdef12-1234-1234-1234-123456789abc',
  prefixDigest:'a'.repeat(64),checksum:'b'.repeat(64),payload:{ui:{}}})));
const options={thresholdMs:100,minimumSamples:1,metric:'durable-decision-ack',buildKind:'unit-fixture',boundaryDefinition:'Actual ACK duration unchanged by partitioning'};
test('checkpoint due is relative to the last checkpoint, not revision modulo 50',()=>{
  assert.equal(checkpointExpectation(3,49,50).expected,false);
  assert.equal(checkpointExpectation(3,52,53).expected,true);
  assert.equal(checkpointExpectation(53,99,100).expected,false);
  assert.equal(checkpointExpectation(53,102,103).expected,true);
});
test('due checkpoints require exact ACK revision and unchanged session/log lineage',()=>{
  const prior=metadata(3),current=metadata(53);
  assert.equal(verifyCheckpointChange(prior,current,53).observedChanged,true);
  assert.throws(()=>verifyCheckpointChange(prior,current,54),/exact acknowledged/);
  assert.throws(()=>verifyCheckpointChange(prior,{...current,sessionId:'another'},53),/native session/);
  assert.throws(()=>verifyCheckpointChange(prior,{...current,logGeneration:'another'},53),/log generation/);
  assert.throws(()=>verifyCheckpointChange(prior,prior,3),/did not advance/);
});
test('malformed or unsafe checkpoint revisions are never classified as evidence',()=>{
  assert.throws(()=>checkpointMetadata(Buffer.from('{}')),/actual native checkpoint schema/);
  assert.throws(()=>metadata(Number.MAX_SAFE_INTEGER+1),/exact safe integer/);
  assert.throws(()=>metadata(-1),/exact safe integer/);
  assert.throws(()=>checkpointMetadata(Buffer.from([0xff])),/encoded data/);
  assert.throws(()=>checkpointExpectation(5,4,5),/ahead/);
  assert.throws(()=>checkpointExpectation(0,4,6),/exactly one/);
});
test('all attempts remain partitioned and overall p95 is unchanged while checkpoint cost stays separate',()=>{
  const ordinary={actionId:'ordinary',phase:'measure',outcome:'success',elapsedMs:5,checkpoint:checkpointExpectation(3,10,11)};
  const expected=checkpointExpectation(3,52,53);
  const verified={actionId:'checkpoint',phase:'measure',outcome:'success',elapsedMs:300,checkpoint:{...expected,...verifyCheckpointChange(metadata(3),metadata(53),53),observationMs:900}};
  const failed={actionId:'failed',phase:'measure',outcome:'rejected',elapsedMs:90,checkpoint:expected};
  const unknown={actionId:'unknown',phase:'measure',outcome:'ownership-unresolved',elapsedMs:null};
  const samples=[ordinary,verified,failed,unknown],before=summarizeLatency(samples,options);
  const result=checkpointSubsets(samples,options);
  assert.equal(result.partitionedAttempts,4);assert.equal(result.ordinary.totalAttempts,1);assert.equal(result.checkpointChanging.totalAttempts,1);
  assert.equal(result.checkpointChanging.p95SuccessfulOnlyMs,300);assert.equal(result.ordinary.p95SuccessfulOnlyMs,5);
  assert.equal(result.dueUnverified.count,1);assert.equal(result.unclassified.count,1);
  assert.deepEqual(summarizeLatency(samples,options),before);assert.equal(before.totalAttempts,4);assert.equal(before.observedThreshold,'incomplete');
});
