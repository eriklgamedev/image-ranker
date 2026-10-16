import {performance} from 'node:perf_hooks';
import fs from 'node:fs';
import {randomUUID} from 'node:crypto';

const metrics = new Set(['command-ack','durable-decision-ack','visible-presentation','restore-ready','cancel-ack','cancel-to-reap']);
const finite = (value,min,max,name) => {
  if (!Number.isFinite(value)||value<min||value>max) throw new Error(`Invalid ${name}`);
};
const boundedText = text => String(text).slice(0,500);

export function summarizeLatency(samples, {thresholdMs,minimumSamples=20,buildKind,metric,boundaryDefinition}) {
  if (!Array.isArray(samples)||samples.length>10000) throw new Error('Invalid samples');
  const measured=samples.filter(sample=>sample.phase==='measure');
  const valid=measured.filter(sample=>sample.outcome==='success');
  const sorted=valid.map(sample=>sample.elapsedMs).sort((a,b)=>a-b);
  for (const value of sorted) finite(value,0,86400000,'duration');
  const p95=sorted.length?sorted[Math.ceil(sorted.length*.95)-1]:null;
  const failures=measured.filter(sample=>sample.outcome!=='success');
  const warmupFailures=samples.filter(sample=>sample.phase==='warmup'&&sample.outcome!=='success');
  const complete=valid.length>=minimumSamples&&!failures.length&&!warmupFailures.length;
  return {metric,boundaryDefinition,buildKind,totalAttempts:measured.length,successfulAttempts:valid.length,
    warmupAttempts:samples.length-measured.length,failedAttempts:failures.length,warmupFailures:warmupFailures.length,
    p95SuccessfulOnlyMs:p95,thresholdMs,minimumSamples,
    observedThreshold:!complete?'incomplete':p95<=thresholdMs?'met':'exceeded',
    releaseQualification:'not-established-by-this-summary',
    percentileMethod:'nearest-rank; failures retained separately and prevent a passing threshold result'};
}

/**
 * Adapter-owned actual action, returning its native/UI result; verify must bind
 * that result to the exact event/request and measurement boundary. No fake busy
 * loop, process control or hidden app endpoint is provided here.
 * A timed-out action is retained as pendingAction if bounded drain did not finish.
 * The caller must retain its real operation/process ownership until settlement.
 */
export async function measureLatency({action,verify,metric,boundaryDefinition,buildKind,
  count=100,warmup=5,thresholdMs=250,minimumSamples=20,actionTimeoutMs=5000,
  drainTimeoutMs=5000,maxRunMs=120000,signal,now=()=>performance.now()}) {
  if (typeof action!=='function'||typeof verify!=='function') throw new Error('Actual action and semantic verifier required');
  if (!metrics.has(metric)||!['debug-e2e','installed-release','unit-fixture'].includes(buildKind)) throw new Error('Explicit supported metric/build kind required');
  if (typeof boundaryDefinition!=='string'||boundaryDefinition.length<10||boundaryDefinition.length>1000) throw new Error('Explicit boundary definition required');
  for (const [name,value,min,max] of [['count',count,1,10000],['warmup',warmup,0,100],['threshold',thresholdMs,0,60000],['minimumSamples',minimumSamples,1,10000],['actionTimeoutMs',actionTimeoutMs,1,60000],['drainTimeoutMs',drainTimeoutMs,1,30000],['maxRunMs',maxRunMs,1,3600000]]) finite(value,min,max,name);
  if (![count,warmup,minimumSamples].every(Number.isInteger)) throw new Error('Counts must be integers');
  const runId=randomUUID(), samples=[],started=now();
  let pendingAction=null,stopReason='completed';
  for(let index=0;index<count+warmup;index++) {
    if(signal?.aborted||now()-started>=maxRunMs) {stopReason=signal?.aborted?'cancelled':'run-deadline';break;}
    const phase=index<warmup?'warmup':'measure';
    const actionId=`${runId}:${index}`,controller=new AbortController(),start=now();
    let timeout,abortListener;
    const operation=Promise.resolve().then(()=>action({actionId,signal:controller.signal})).then(
      value=>({kind:'settled',ok:true,value,ended:now()}),
      error=>({kind:'settled',ok:false,error:boundedText(error?.message??error),ended:now()}));
    const interruption=new Promise(resolve=>{
      timeout=setTimeout(()=>resolve({kind:'timeout'}),Math.max(1,Math.min(actionTimeoutMs,maxRunMs-(now()-started))));
      abortListener=()=>resolve({kind:'cancelled'});
      signal?.addEventListener('abort',abortListener,{once:true});
      if(signal?.aborted)abortListener();
    });
    const result=await Promise.race([operation,interruption]);
    clearTimeout(timeout);signal?.removeEventListener('abort',abortListener);
    if(result.kind!=='settled') {
      controller.abort(result.kind);
      let drainTimer;
      const drained=await Promise.race([operation,new Promise(resolve=>{drainTimer=setTimeout(()=>resolve(null),drainTimeoutMs);})]);
      clearTimeout(drainTimer);
      samples.push({actionId,phase,outcome:result.kind,elapsedMs:now()-start,settledAfterAbort:drained!==null});
      if(!drained)pendingAction=operation;
      stopReason=drained?result.kind:'action-ownership-unresolved';
      break; // Never start another action while cancellation/ownership is uncertain.
    }
    let outcome=result.ok?'success':'error',reason=result.error;
    if(result.ok) {
      try {
        // A synchronous verifier keeps its CPU work out of the measured ACK time.
        const verified=verify(result.value,{actionId,metric,phase});
        if(verified&&typeof verified.then==='function') {
          // Reject async semantics without leaving a rejected verifier promise
          // unobserved; the adapter must settle all native evidence in action().
          Promise.resolve(verified).catch(()=>{});
          throw new Error('Semantic verifier must be synchronous');
        }
        if(verified!==true)throw new Error('Semantic verifier did not return true');
      } catch(error) {outcome='rejected';reason=boundedText(error.message);}
    }
    samples.push({actionId,phase,outcome,elapsedMs:result.ended-start,...(reason?{reason}:{})});
    if(outcome!=='success'){stopReason='action-failed';break;}
  }
  const summary=summarizeLatency(samples,{thresholdMs,minimumSamples,buildKind,metric,boundaryDefinition});
  if(stopReason!=='completed'||samples.length!==count+warmup)summary.observedThreshold='incomplete';
  return {runId,stopReason,summary,samples,pendingAction};
}

export function writeLatencyResult(file,result) {
  const body=JSON.stringify({runId:result.runId,stopReason:result.stopReason,summary:result.summary,
    samples:result.samples,unresolvedAction:result.pendingAction!==null},null,2)+'\n';
  if(Buffer.byteLength(body)>8*1024*1024)throw new Error('Result exceeds evidence bound');
  const fd=fs.openSync(file,'wx');
  try{fs.writeFileSync(fd,body);fs.fsyncSync(fd);}finally{fs.closeSync(fd);}
}
