// Reusable Windows qualification; excluded from the installed application.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {randomUUID} from 'node:crypto';
import {digest} from './fixtures.mjs';

const root=process.env.IMAGE_RANKER_E2E_ROOT;
const inventory=JSON.parse(fs.readFileSync(path.join(root,'inventory.json'),'utf8'));
const requestFile=path.join(root,'recovery-worker-hold.json');
const control=path.join(root,'launcher','recovery-protection-control.v2');
async function native(command,args={}) {
  const {result:response}=await browser.executeAsync((cmd,input,done)=>{
    void (async()=>{
      if(!['get_boot_status','preview_context'].includes(cmd)&&!Object.hasOwn(input,'contextId'))
        input={...input,contextId:await window.__TAURI__.core.invoke('preview_context')};
      return window.__TAURI__.core.invoke(cmd,input);
    })().then(value=>done({result:{ok:true,value}}),error=>done({result:{ok:false,error:String(error)}}));
  },command,args);
  if(!response.ok)throw new Error(`${command}: ${response.error}`);
  return response.value;
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
  await browser.waitUntil(()=>browser.execute(name=>!!window[name]&&window[name].state!=='pending',key));
  const {result}=await browser.execute(name=>({result:window[name]}),key);
  assert.ok(result);
  return result;
}
async function idle() { await browser.waitUntil(async()=>!(await native('recovery_work_status')).active); }
async function saved() { await idle();await browser.waitUntil(async()=>(await native('session_save_status')).state==='saved'); }
function sourcesUnchanged() { for(const entry of inventory)assert.equal(digest(entry.path),entry.hash); }
function publish(file,value) {
  const pending=`${file}.pending`;
  fs.writeFileSync(pending,JSON.stringify(value,null,2),{flag:'wx',flush:true});
  fs.linkSync(pending,file);fs.unlinkSync(pending);
}
async function arm(kind,phase) {
  const gate={nonce:randomUUID(),contextId:await native('preview_context'),kind,phase,released:false};
  fs.writeFileSync(requestFile,JSON.stringify({kind,phase,nonce:gate.nonce,contextId:gate.contextId}),{flag:'wx',flush:true});
  return gate;
}
async function held(gate) {
  const file=path.join(root,`recovery-held-${gate.nonce}.json`);
  await browser.waitUntil(()=>fs.existsSync(file),{timeout:20000,timeoutMsg:'Exact Windows worker boundary was not reached'});
  const evidence=JSON.parse(fs.readFileSync(file,'utf8'));
  const status=await native('recovery_work_status');
  assert.equal(evidence.nonce,gate.nonce);
  assert.equal(evidence.boundary,'native-progress-after-lock-release');
  assert.equal(evidence.job.contextId,gate.contextId);
  assert.equal(evidence.job.kind,gate.kind);assert.equal(evidence.job.phase,gate.phase);
  assert.equal(status.active.id,evidence.job.id);
  return evidence;
}
function release(gate,evidence) {
  if(gate.released)return;
  assert.equal(evidence.nonce,gate.nonce);assert.equal(evidence.job.contextId,gate.contextId);
  publish(path.join(root,`recovery-release-${gate.nonce}.json`),{nonce:gate.nonce,jobId:evidence.job.id});
  gate.released=true;
}
async function cleanup(gate,evidence) {
  // Even a failed assertion must release this exact worker before starting another.
  const file=path.join(root,`recovery-held-${gate.nonce}.json`);
  const retained=evidence||(fs.existsSync(file)?JSON.parse(fs.readFileSync(file,'utf8')):null);
  if(retained)release(gate,retained);
  if(fs.existsSync(requestFile)) {
    const request=JSON.parse(fs.readFileSync(requestFile,'utf8'));
    assert.equal(request.nonce,gate.nonce);fs.unlinkSync(requestFile);
  }
  await idle();
}
async function helpAtBoundary() {
  const button=await $('[aria-label="Help and keyboard shortcuts"]');
  await button.waitForEnabled();await button.click();
  await $('[aria-label="Recovery work controls"]').waitForDisplayed();
  await $('//h3[starts-with(normalize-space(),"About Image Ranker")]').waitForDisplayed();
  const info=await native('app_info');assert.ok(info.version);
}
async function cancelHeld(gate,evidence) {
  const ack=await native('cancel_recovery_work',{jobId:evidence.job.id});
  assert.equal(ack.id,evidence.job.id);assert.equal(ack.cancelRequested,true);
  const status=await native('recovery_work_status');
  assert.equal(status.active.id,evidence.job.id);assert.equal(status.active.cancelRequested,true);
  release(gate,evidence);
}

describe('Windows worker coordination and protection maintenance',()=>{
  before(async()=>{await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();await saved();});

  it('keeps retained session and Help available during explicit startup history retry',async()=>{
    const before=await native('load_session_state');
    const gate=await arm('startup-load','loading-operation-history');let evidence;
    try {
      await start('retry_boot',{contextId:gate.contextId},'windowsStartupRetry');evidence=await held(gate);
      assert.deepEqual(await native('load_session_state'),before);
      assert.equal((await native('get_boot_status')).contextId,gate.contextId);
      await helpAtBoundary();
      // The empty retained workspace has no UI snapshot. Pass the command's version
      // admission check so this assertion exercises the actual active-worker guard.
      await assert.rejects(()=>native('save_session',{value:before.session||{schemaVersion:1}}),/RECOVERY_WORK_BUSY/);
      await cancelHeld(gate,evidence);
      const result=await outcome('windowsStartupRetry');assert.equal(result.state,'rejected');assert.match(result.error,/RECOVERY_CANCELLED/);
      await idle();assert.deepEqual(await native('load_session_state'),before);sourcesUnchanged();
      publish(path.join(root,'windows-startup-worker-evidence.json'),{gate,evidence,result,retained:before});
    } finally {await cleanup(gate,evidence);}
    const retried=await native('retry_boot');assert.equal(retried.mode,'ready');await saved();
    await $('button=Done').click();
  });

  it('cancels profile preparation before the picker without losing the current session',async()=>{
    const before=await native('load_session_state'),boot=await native('get_boot_status');
    const gate=await arm('prepare-profile','checking-current-save');let evidence;
    try {
      await start('choose_recovery_storage',{contextId:gate.contextId,expectedCurrentSession:before.session},'windowsProfilePrepare');
      evidence=await held(gate);await helpAtBoundary();
      assert.deepEqual(await native('load_session_state'),before);
      await cancelHeld(gate,evidence);
      const result=await outcome('windowsProfilePrepare');assert.equal(result.state,'rejected');assert.match(result.error,/RECOVERY_CANCELLED/);
      await idle();assert.equal((await native('get_boot_status')).profile.id,boot.profile.id);
      assert.deepEqual(await native('load_session_state'),before);sourcesUnchanged();
      publish(path.join(root,'windows-profile-worker-evidence.json'),{gate,evidence,result,beforeProfile:boot.profile});
    } finally {await cleanup(gate,evidence);}
    await $('button=Done').click();
  });

  it('cancels protection preview while keeping status and the durable head unchanged',async()=>{
    const before=digest(control),session=await native('load_session_state');
    const gate=await arm('preview-registry-maintenance','reading-protection-history');let evidence;
    try {
      await start('preview_protection_maintenance',{contextId:gate.contextId},'windowsProtectionPreview');evidence=await held(gate);
      await helpAtBoundary();assert.deepEqual(await native('load_session_state'),session);
      const status=await native('protection_maintenance_status');assert.equal(status.active,true);assert.equal(status.fresh,false);
      await cancelHeld(gate,evidence);
      const result=await outcome('windowsProtectionPreview');assert.equal(result.state,'rejected');assert.match(result.error,/RECOVERY_CANCELLED/);
      await idle();assert.equal(digest(control),before);sourcesUnchanged();
      publish(path.join(root,'windows-protection-preview-evidence.json'),{gate,evidence,result,headSha256:before});
    } finally {await cleanup(gate,evidence);}
    await $('button=Done').click();
  });

  it('retains an exact completed protection request after real renderer reload',async()=>{
    const preview=await native('preview_protection_maintenance');
    assert.match(preview.requestId,/^[a-f0-9]{64}$/);
    const gate=await arm('registry-maintenance','validating-maintenance');let evidence;
    try {
      await start('confirm_protection_maintenance',{contextId:gate.contextId,token:preview.token},'windowsProtectionConfirm');
      evidence=await held(gate);
      await browser.refresh();await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();
      const current=await native('preview_context');assert.notEqual(current,gate.contextId);
      release(gate,evidence);await idle();
      // Renderer retirement occurred before the seal, so this request cannot be silently committed.
      const status=await native('protection_maintenance_status',{requestId:preview.requestId});assert.equal(status.result,null);
      const refreshed=await native('preview_protection_maintenance');
      const result=await native('confirm_protection_maintenance',{token:refreshed.token});
      assert.equal(result.requestId,refreshed.requestId);assert.ok(['completed','no-reclaim'].includes(result.state));
      const head=digest(control);await browser.refresh();await saved();
      const retained=await native('protection_maintenance_status',{requestId:result.requestId});assert.deepEqual(retained.result,result);
      assert.equal(digest(control),head,'Exact result observation must not perform a second maintenance action');
      const wrong=await native('protection_maintenance_status',{requestId:'f'.repeat(64)});assert.equal(wrong.result,null);
      assert.equal(wrong.registry.lastResult,null);sourcesUnchanged();
      publish(path.join(root,'windows-protection-reload-evidence.json'),{gate,evidence,retiredRequest:preview.requestId,result,headSha256:head});
    } finally {await cleanup(gate,evidence);}
  });
});
