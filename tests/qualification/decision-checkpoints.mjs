// Observational fixture support, not native session authority or checkpoint repair.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {summarizeLatency} from './latency.mjs';

export const checkpointByteLimit=128*1024*1024; // sessions.rs MAX_SNAPSHOT_BYTES
export const checkpointInterval=50; // persist_admitted: revision - last checkpoint >= 50
const revision=value=>Number.isSafeInteger(value)&&value>=0;
const uuid=value=>typeof value==='string'&&/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(value);
export function checkpointMetadata(bytes) {
  assert.ok(bytes instanceof Uint8Array && bytes.byteLength>0 && bytes.byteLength<=checkpointByteLimit,'Primary checkpoint must fit the native snapshot limit');
  const value=JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(bytes));
  assert.equal(value.schemaVersion,2,'Expected the actual native checkpoint schema');
  assert.ok(revision(value.revision),'Checkpoint revision must be an exact safe integer');
  assert.ok(uuid(value.sessionId)&&uuid(value.logGeneration),'Checkpoint lineage is missing or malformed');
  assert.ok(typeof value.prefixDigest==='string'&&/^[a-f0-9]{64}$/.test(value.prefixDigest));
  assert.ok(typeof value.checksum==='string'&&/^[a-f0-9]{64}$/.test(value.checksum));
  assert.ok(value.payload&&typeof value.payload==='object'&&!Array.isArray(value.payload),'Checkpoint payload must be an object');
  return {revision:value.revision,sessionId:value.sessionId,logGeneration:value.logGeneration,prefixDigest:value.prefixDigest,
    checksum:value.checksum,bytes:bytes.byteLength,sha256:createHash('sha256').update(bytes).digest('hex'),
    qualification:'Observed primary bytes/schema/lineage only; native checksum and replay authority are not revalidated by this fixture.'};
}
export function checkpointExpectation(priorCheckpointRevision,priorDurableRevision,nextRevision) {
  assert.ok([priorCheckpointRevision,priorDurableRevision,nextRevision].every(revision));
  assert.ok(priorCheckpointRevision<=priorDurableRevision,'Checkpoint is ahead of its acknowledged history');
  assert.equal(nextRevision,priorDurableRevision+1,'One decision must add exactly one revision');
  const expected=nextRevision-priorCheckpointRevision>=checkpointInterval;
  return {prior:priorCheckpointRevision,expected,expectedRevision:expected?nextRevision:priorCheckpointRevision,
    after:null,observedChanged:false,observation:expected?'due-unobserved':'ordinary-policy-inferred'};
}
export function verifyCheckpointChange(previous,current,acknowledgedRevision) {
  assert.equal(current.sessionId,previous.sessionId,'Checkpoint changed native session');
  assert.equal(current.logGeneration,previous.logGeneration,'Checkpoint changed native log generation');
  assert.equal(current.revision,acknowledgedRevision,'A due checkpoint must match the exact acknowledged revision');
  assert.ok(current.revision>previous.revision,'The observed primary checkpoint did not advance');
  return {after:current.revision,observedChanged:true,observation:'primary-revision-change-verified'};
}
export function checkpointSubsets(samples,options) {
  const ordinary=samples.filter(sample=>sample.checkpoint?.expected===false);
  const verified=samples.filter(sample=>sample.checkpoint?.observedChanged===true);
  const dueUnverified=samples.filter(sample=>sample.checkpoint?.expected===true&&!sample.checkpoint.observedChanged);
  const unclassified=samples.filter(sample=>!sample.checkpoint);
  const partitionedAttempts=ordinary.length+verified.length+dueUnverified.length+unclassified.length;
  assert.equal(partitionedAttempts,samples.length,'Checkpoint categories must retain every attempt exactly once');
  return {
    ordinary:{classification:'Policy-inferred ordinary ACKs; no primary read after each ordinary ACK, so external checkpoint changes are not excluded.',...summarizeLatency(ordinary,options)},
    checkpointChanging:{classification:'Exact acknowledged revisions verified against the primary checkpoint outside timed ACK intervals.',...summarizeLatency(verified,options)},
    dueUnverified:{count:dueUnverified.length,actionIds:dueUnverified.map(sample=>sample.actionId)},
    unclassified:{count:unclassified.length,actionIds:unclassified.map(sample=>sample.actionId)},
    partitionedAttempts,
  };
}
