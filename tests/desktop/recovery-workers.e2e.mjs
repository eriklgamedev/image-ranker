import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {randomUUID} from 'node:crypto';
import {digest} from './fixtures.mjs';

const root=process.env.IMAGE_RANKER_E2E_ROOT;
const originals=JSON.parse(fs.readFileSync(path.join(root,'inventory.json'),'utf8'));
const requestFile=path.join(root,'recovery-worker-hold.json');
async function native(command,args={}) {
  const {result}=await browser.executeAsync((cmd,input,done)=>{
    void (async()=>{
      if(!['get_boot_status','preview_context'].includes(cmd)&&!Object.hasOwn(input,'contextId'))
        input={...input,contextId:await window.__TAURI__.core.invoke('preview_context')};
      return window.__TAURI__.core.invoke(cmd,input);
    })().then(value=>done({result:{ok:true,value}}),error=>done({result:{ok:false,error:String(error)}}));
  },command,args);
  if(!result.ok)throw new Error(`${command}: ${result.error}`);
  return result.value;
}
async function start(command,args,key) {
  await browser.execute((cmd,input,name)=>{
    window[name]={state:'pending'};
    void window.__TAURI__.core.invoke(cmd,input).then(
      value=>{window[name]={state:'resolved',value};},
      error=>{window[name]={state:'rejected',error:String(error)};},
    );
  },command,args,key);
}
async function outcome(key) {
  await browser.waitUntil(()=>browser.execute(name=>window[name]?.state!=='pending',key));
  const {result}=await browser.execute(name=>({result:window[name]}),key);
  assert.ok(result);
  return result;
}
async function idle() {
  await browser.waitUntil(async()=>!(await native('recovery_work_status')).active);
}
async function saved() {
  await browser.waitUntil(async()=>(await native('session_save_status')).state==='saved');
}
async function help() {
  if(!await $('[aria-label="Recovery work controls"]').isDisplayed())
    await $('[aria-label="Help and keyboard shortcuts"]').click();
  await $('[aria-label="Recovery work controls"]').waitForDisplayed();
}
function sourceBytes() {
  for(const original of originals)assert.equal(digest(original.path),original.hash);
}
async function arm(kind,phase,minimumCompleted=0) {
  const nonce=randomUUID(),contextId=await native('preview_context');
  fs.writeFileSync(requestFile,JSON.stringify({kind,phase,minimumCompleted,nonce,contextId}),{flag:'wx'});
  return {nonce,contextId};
}
async function held(gate) {
  const evidencePath=path.join(root,`recovery-held-${gate.nonce}.json`);
  await browser.waitUntil(()=>fs.existsSync(evidencePath),{timeout:20000,timeoutMsg:'Expected exact native recovery boundary evidence'});
  const evidence=JSON.parse(fs.readFileSync(evidencePath,'utf8'));
  const status=await native('recovery_work_status');
  assert.equal(evidence.nonce,gate.nonce);
  assert.equal(evidence.boundary,'native-progress-after-lock-release');
  assert.equal(status.active.id,evidence.job.id);
  assert.equal(status.active.contextId,gate.contextId);
  assert.equal(status.active.phase,evidence.job.phase);
  return evidence;
}
function release(gate,evidence) {
  // FIX 1: Native consumes the release file. Never recreate it in finally.
  if(gate.released)return;
  assert.equal(evidence.nonce,gate.nonce,'Release evidence must match this exact gate');
  assert.equal(evidence.job.contextId,gate.contextId,'Release must target the admitted context');
  assert.ok(typeof evidence.job.id==='string'&&evidence.job.id.length>0);
  const file=path.join(root,`recovery-release-${gate.nonce}.json`);
  const message={nonce:gate.nonce,jobId:evidence.job.id};
  if(fs.existsSync(file))assert.deepEqual(JSON.parse(fs.readFileSync(file,'utf8')),message);
  else {
    const pending=path.join(root,`recovery-release-pending-${gate.nonce}.json`);
    fs.writeFileSync(pending,JSON.stringify(message),{flag:'wx',flush:true});
    fs.linkSync(pending,file);
    fs.unlinkSync(pending);
  }
  gate.released=true;
}
async function cleanupGate(gate,evidence,originalFailure,extraFiles=[]) {
  const errors=[];
  const attempt=async(name,action)=>{
    try { await action(); }
    catch(error) { errors.push({step:name,message:String(error),stack:error?.stack}); }
  };
  // FIX 2: held() can read native evidence and then fail an assertion before
  // returning it. Recover only this gate's nonce-qualified evidence to release
  // its exact worker before waiting for idle. Never cancel a different job.
  await attempt('release exact held worker',()=>{
    if(!gate||gate.released)return;
    const file=path.join(root,`recovery-held-${gate.nonce}.json`);
    const retained=evidence||(fs.existsSync(file)?JSON.parse(fs.readFileSync(file,'utf8')):null);
    if(retained)release(gate,retained);
  });
  await attempt('remove unconsumed hold request',()=>{
    if(!gate||!fs.existsSync(requestFile))return;
    const request=JSON.parse(fs.readFileSync(requestFile,'utf8'));
    assert.equal(request.nonce,gate.nonce,'Cleanup must not remove another gate request');
    assert.equal(request.contextId,gate.contextId);
    fs.unlinkSync(requestFile);
  });
  for(const file of extraFiles)await attempt(`remove ${path.basename(file)}`,()=>{
    if(fs.existsSync(file))fs.unlinkSync(file);
  });
  await attempt('wait for actual native worker exit',idle);
  // FIX 3: Cleanup failures remain evidence, but the original assertion is the
  // test failure when both occur. A clean assertion path must fail if cleanup
  // cannot establish the worker's exit; it must not be reported as a pass.
  const record={nonce:gate?.nonce||null,released:gate?.released||false,originalFailure:originalFailure?{message:String(originalFailure),stack:originalFailure.stack}:null,errors};
  try { fs.appendFileSync(path.join(root,'recovery-cleanup-evidence.jsonl'),JSON.stringify(record)+'\n'); }
  catch(error) { errors.push({step:'record cleanup evidence',message:String(error),stack:error?.stack}); }
  if(errors.length) {
    console.error('Recovery fixture cleanup did not complete:',JSON.stringify(record));
    if(!originalFailure)throw new AggregateError(errors.map(error=>new Error(`${error.step}: ${error.message}`)),'Recovery fixture cleanup did not establish a safe terminal state');
  }
}
async function refreshControls() {
  const controls=await $('[aria-label="Recovery work controls"]');
  await controls.$('button=Refresh recovery work status').waitForEnabled();
  await controls.$('button=Refresh recovery work status').click();
  return controls;
}
async function observeWhileHeld(evidence) {
  const samples=[];
  for(const command of ['get_boot_status','load_session_state','session_save_status','recovery_work_status']) {
    const started=performance.now(),value=await native(command);
    samples.push({command,milliseconds:performance.now()-started});
    assert.ok(value);
  }
  assert.ok(samples.every(sample=>sample.milliseconds<1000),'Independent observations must not wait for explicit held-worker release');
  assert.equal((await native('recovery_work_status')).active.id,evidence.job.id);
  return samples;
}
async function inspected() {
  await browser.waitUntil(async()=>(await native('image_work_status')).active===0);
  return native('inspect_recovery_copy');
}
async function replacement(preview) {
  return native('prepare_recovery_replacement',{subject:{kind:'fresh',token:preview.token,acceptPending:false}});
}
function restoreArgs(preview,prepared,contextId,disposition='saved') {
  return {token:preview.token,acceptPending:false,replacementToken:prepared.token,contextId,disposition};
}
function record(name,value) {
  fs.writeFileSync(path.join(root,`${name}-evidence.json`),JSON.stringify(value,null,2));
}

describe('Owned recovery work in the actual Windows WebView',()=>{
  before(async()=>{
    await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();
    await saved();
    await $('button=Add folder').click();
    await browser.waitUntil(async()=>(await native('load_session')).report.images.length===20);
    await saved();
    await browser.waitUntil(async()=>(await native('image_work_status')).active===0);
    const copy=await native('export_session_copy',{value:await native('load_session')});
    assert.ok(copy);
    assert.ok(fs.existsSync(path.join(root,'session-recovery.json')));
    await help();
  });

  for(const reload of [false,true])it(`retains an actual open inspection file until safe cancellation exits${reload?' across renderer retirement':''}`,async()=>{
    await idle();
    const prior=await inspected(),before=await native('load_session_state');
    const gate=await arm('inspect-copy','reading-copy',1),key=`inspection-${gate.nonce}`;
    let evidence,originalFailure;
    try {
      await start('inspect_recovery_copy',{contextId:gate.contextId},key);
      evidence=await held(gate);
      assert.ok(evidence.job.completed>0,'This barrier must be inside the opened-file chunk reader');
      const samples=await observeWhileHeld(evidence);
      if(reload) {
        await browser.refresh();
        await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();
        assert.notEqual(await native('preview_context'),gate.contextId);
        await help();
      }
      const ack=await native('cancel_recovery_work',{jobId:evidence.job.id});
      assert.equal(ack.id,evidence.job.id);assert.equal(ack.cancelRequested,true);
      const pending=await native('recovery_work_status');
      assert.equal(pending.active.id,evidence.job.id);assert.equal(pending.active.state,'cancelling');
      const controls=await refreshControls();
      await browser.waitUntil(async()=>(await controls.getText()).includes('Cancellation requested'));
      assert.deepEqual((await native('load_session_state')).session,before.session);
      sourceBytes();
      release(gate,evidence);
      await idle();
      const terminal=(await native('recovery_work_status')).recent.find(job=>job.id===evidence.job.id);
      assert.equal(terminal.state,'cancelled');
      if(!reload) {
        const response=await outcome(key);assert.equal(response.state,'rejected');assert.match(response.error,/RECOVERY_CANCELLED/);
        assert.ok(await replacement(prior),'Cancellation preserves the prior inspected subject');
      } else await assert.rejects(()=>replacement(prior),/STALE_RECOVERY_PREVIEW|STALE_CONTEXT/);
      assert.ok(await inspected(),'New work can start only after the old worker exits');
      record(`inspection-${reload?'reload':'current'}`,{gate,evidence,samples,ack,pending,terminal,beforeContext:before.contextId});
    } catch(error) {
      originalFailure=error;throw error;
    } finally {
      await cleanupGate(gate,evidence,originalFailure);
    }
  });

  it('preserves the current failed-save action when a staged restore is cancelled before activation',async()=>{
    await idle();
    const preview=await inspected(),original=await native('load_session'),marker=path.join(root,'fail-session-append');
    const proposal=structuredClone(original);proposal.name='Pending work must survive cancelled restore';
    fs.writeFileSync(marker,'image-ranker-e2e-injected-append-failure\n',{flag:'wx'});
    let gate,evidence,originalFailure;
    try {
      await assert.rejects(()=>native('save_session',{value:proposal}),/save_session: STORAGE: injected desktop session append failure \[stage=append\.pre-admission\]/);
      const before=await native('load_session_state');
      assert.equal(before.saveStatus.state,'pending');assert.ok(before.saveStatus.pendingEventId);
      // The injected append fault is global to the disposable fixture. Its
      // purpose is to create the real retained proposal, not to fail the new
      // branch's bootstrap writes. Removing it does not retry the old proposal.
      fs.unlinkSync(marker);
      assert.equal((await native('session_save_status')).pendingEventId,before.saveStatus.pendingEventId);
      const prepared=await replacement(preview);
      assert.equal(prepared.requiresDisposition,true);
      gate=await arm('restore-copy','prepare-activation');
      const key=`restore-${gate.nonce}`,args=restoreArgs(preview,prepared,gate.contextId,'discard');
      await start('restore_recovery_copy',args,key);
      evidence=await held(gate);
      const samples=await observeWhileHeld(evidence);
      const ack=await native('cancel_recovery_work',{jobId:evidence.job.id});
      assert.equal(ack.cancelRequested,true);
      assert.equal((await native('recovery_work_status')).active.id,evidence.job.id);
      release(gate,evidence);await idle();
      const response=await outcome(key);assert.equal(response.state,'rejected');assert.match(response.error,/RECOVERY_CANCELLED/);
      const after=await native('load_session_state');
      assert.deepEqual(after.session,before.session);assert.equal(after.sessionGeneration,before.sessionGeneration);
      assert.equal(after.saveStatus.pendingEventId,before.saveStatus.pendingEventId);
      assert.equal(after.saveStatus.durableRevision,before.saveStatus.durableRevision);
      sourceBytes();
      record('restore-before-seal',{gate,evidence,samples,ack,response,before,after});
    } catch(error) {
      originalFailure=error;throw error;
    } finally {
      await cleanupGate(gate,evidence,originalFailure,[marker]);
    }
    await native('save_session',{value:proposal});await saved();
    await browser.refresh();await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();await saved();await help();
  });

  for(const phase of ['committing','published'])it(`finishes a sealed restore through reload at ${phase} and returns its exact result once`,async()=>{
    await idle();
    const preview=await inspected(),prepared=await replacement(preview),before=await native('load_session_state');
    const gate=await arm('restore-copy',phase),args=restoreArgs(preview,prepared,gate.contextId),key=`sealed-${gate.nonce}`;
    let evidence,originalFailure;
    try {
      await start('restore_recovery_copy',args,key);evidence=await held(gate);
      const samples=await observeWhileHeld(evidence);
      await assert.rejects(()=>native('cancel_recovery_work',{jobId:evidence.job.id}),/RECOVERY_JOB_FINISHING/);
      await browser.refresh();await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();await help();
      assert.notEqual(await native('preview_context'),gate.contextId);
      const controls=await refreshControls();
      await browser.waitUntil(async()=>(await controls.getText()).includes('Activation is finishing'));
      assert.equal(await controls.$('button=Cancel recovery work').isEnabled(),false);
      let earlyRetry;
      if(phase==='published') {
        earlyRetry=await native('restore_recovery_copy',args);
        assert.equal((await native('recovery_work_status')).active.id,evidence.job.id);
      }
      release(gate,evidence);await idle();
      const terminal=(await native('recovery_work_status')).recent.find(job=>job.id===evidence.job.id);
      assert.equal(terminal.state,'completed');
      const once=await native('restore_recovery_copy',args),again=await native('restore_recovery_copy',args);
      assert.deepEqual(again,once);
      if(earlyRetry)assert.deepEqual(earlyRetry,once);
      await assert.rejects(()=>native('restore_recovery_copy',{...args,disposition:'discard'}),/STALE_CONTEXT|STALE_RECOVERY|RECOVERY_/);
      const after=await native('load_session_state');
      assert.notEqual(after.sessionGeneration,before.sessionGeneration);assert.equal(after.saveStatus.state,'saved');
      const stateBeforeRetry=structuredClone(after);
      await native('restore_recovery_copy',args);
      assert.deepEqual(await native('load_session_state'),stateBeforeRetry,'Lost responses must not create another branch or revision');
      sourceBytes();
      record(`restore-${phase}`,{gate,evidence,samples,terminal,once,earlyRetry,before,after});
      await browser.refresh();await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();await saved();await help();
    } catch(error) {
      originalFailure=error;throw error;
    } finally {
      await cleanupGate(gate,evidence,originalFailure);
    }
  });
});
