// Actual file commit + post-session-sync panic, exercised only by an e2e build
// inside the native-validated disposable fixture. This is not a power-loss test.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {createHash,randomUUID} from 'node:crypto';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {digest} from './fixtures.mjs';
import {windowsLocationKey} from './windows-paths.mjs';

const root=process.env.IMAGE_RANKER_E2E_ROOT;
assert.ok(root&&path.isAbsolute(root));
assert.equal(fs.readFileSync(path.join(root,'.image-ranker-e2e'),'utf8').trim(),'image-ranker-disposable-fixture-v1');
const inventory=JSON.parse(fs.readFileSync(path.join(root,'inventory.json'),'utf8'));
const copyPath=path.join(root,'session-recovery.json');
const appendMarker=path.join(root,'fail-session-append');
const panicRequest=path.join(root,'session-sync-panic.json');
const barrierRequest=path.join(root,'fault-request.json');
async function native(command,args={}) {
  const {result}=await browser.executeAsync((cmd,input,done)=>{
    void(async()=>{
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
  await browser.waitUntil(()=>browser.execute(name=>!!window[name]&&window[name].state!=='pending',key));
  const {result}=await browser.execute(name=>({result:window[name]}),key);
  assert.ok(result);return result;
}
async function saved() {
  await browser.waitUntil(async()=>(await native('session_save_status')).state==='saved');
}
async function click(text) {const element=await $(`button=${text}`);await element.waitForEnabled();await element.click();}
async function supervise(action,expectedPid) {
  assert.equal(process.platform,'win32');
  const args=['-NoProfile','-File',path.resolve('tests/desktop/kill-app.ps1'),'-DriverId',process.env.IMAGE_RANKER_DRIVER_PID,'-ApplicationPath',path.resolve('src-tauri/target/debug/image-ranker.exe'),'-FixtureRoot',root,'-Action',action];
  if(expectedPid!==undefined)args.push('-ExpectedProcessId',String(expectedPid));
  const {stdout}=await promisify(execFile)('powershell.exe',args,{windowsHide:true,timeout:15000});
  const result=JSON.parse(stdout.trim());
  fs.appendFileSync(path.join(root,'process-supervision.jsonl'),JSON.stringify({...result,observedAt:new Date().toISOString()})+'\n');
  return result;
}
function publish(file,value) {
  const pending=`${file}.${randomUUID()}.pending`;
  fs.writeFileSync(pending,JSON.stringify(value),{flag:'wx',flush:true});
  fs.linkSync(pending,file);fs.unlinkSync(pending);
}
function imageEvidence(plan) {
  const moved=new Map(plan.rows.map(row=>[windowsLocationKey(row.from),row]));
  return inventory.map(original=>{
    const row=moved.get(windowsLocationKey(original.path));
    const current=row?row.to:original.path;
    if(row)assert.equal(fs.existsSync(row.from),false,'Committed rename must have removed the original directory entry');
    assert.equal(digest(current),original.hash,'Every moved or untouched image must retain its bytes');
    const stat=fs.statSync(current,{bigint:true});
    return {original:original.path,current,sha256:original.hash,device:stat.dev.toString(),file:stat.ino.toString(),size:stat.size.toString()};
  });
}
function readJournal(file) {
  const bytes=fs.readFileSync(file),events=[];let offset=0;
  while(offset<bytes.length) {
    assert.ok(bytes.length-offset>=40,'Expected a whole synced session frame');
    assert.equal(bytes.toString('ascii',offset,offset+4),'IRL1');
    const length=bytes.readUInt32LE(offset+4);assert.ok(length<=16*1024*1024&&offset+40+length<=bytes.length);
    const payload=bytes.subarray(offset+40,offset+40+length);
    assert.deepEqual(createHash('sha256').update(payload).digest(),bytes.subarray(offset+8,offset+40));
    events.push(JSON.parse(payload.toString('utf8')));offset+=40+length;
  }
  return {events,bytes:bytes.length,sha256:createHash('sha256').update(bytes).digest('hex')};
}
async function reloadRenderer(previousContext) {
  let warning=null;
  try {await browser.refresh();} catch(error) {
    if(!/unexpected alert open/i.test(String(error))||!await browser.isAlertOpen())throw error;
    warning=String(error);await browser.acceptAlert();
  }
  if(await browser.isAlertOpen())await browser.acceptAlert();
  await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();
  await browser.waitUntil(async()=>(await native('preview_context'))!==previousContext);
  return {context:await native('preview_context'),warning};
}
async function restartIssue() {
  const boot=await native('get_boot_status');
  assert.ok(boot.issues.some(issue=>issue.code==='FILE_OPERATION_RESTART_REQUIRED'),JSON.stringify(boot));
  return boot;
}
async function ordinaryCloseDenied(pid) {
  await browser.executeAsync(done=>{
    window.__restartCloseRequest=null;
    window.__TAURI__.event.listen('close-requested',event=>{window.__restartCloseRequest=event.payload;})
      .then(()=>done({result:true}),error=>done({result:{error:String(error)}}));
  });
  const requested=await supervise('WM_CLOSE',pid);assert.equal(requested.closeRequestedPid,pid);
  await browser.waitUntil(()=>browser.execute(()=>!!window.__restartCloseRequest?.token));
  const {result:close}=await browser.execute(()=>({result:window.__restartCloseRequest}));
  assert.equal(close.contextId,await native('preview_context'));
  const failures=[];
  for(const disposition of ['saved','recovery-copy','discard']) {
    let failure;
    try {await native('confirm_close',{token:close.token,disposition});} catch(error){failure=String(error);}
    assert.ok(failure,'Ordinary close must not release an unverified file-operation owner');
    assert.match(failure,/CLOSE_BUSY|FILE_OPERATION_RESTART_REQUIRED/);failures.push({disposition,error:failure});
  }
  assert.equal((await supervise('inspect',pid)).running,true);
  // Dismiss only the close review; this does not clear the operation restriction.
  const keep=await $('button=Stay open');await keep.waitForEnabled();await keep.click();
  return {requested,close,failures};
}

describe('Preserve work across an unverified file-operation restart',()=>{
  it('keeps native and renderer pending layers separate, verifies the copy, and never repeats committed image moves on reopen',async()=>{
    const record={evidenceClass:'real-rename-post-session-sync-before-ack-caught-panic'};
    const nonce=randomUUID(),gate=randomUUID();let primaryFailure,gateReleased=false,operationStarted=false,plan;
    const readyPath=path.join(root,`barrier-${gate}.ready.json`);
    const release=()=>{if(!gateReleased){fs.writeFileSync(path.join(root,`barrier-${gate}.release`),'release\n',{flag:'wx',flush:true});gateReleased=true;}};
    try {
      await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();await saved();
      await click('Add folder');
      await browser.waitUntil(async()=>(await native('load_session')).report.images.length===20);await saved();
      await click('None');await saved();
      for(const number of ['01','02']){await $(`input[aria-label="Select fixture-${number}.bmp"]`).click();await saved();}
      await $('[aria-label="Ranking mode"]').selectByAttribute('value','full-pairwise');await saved();
      await click('Rank 2 images');await click('Start comparing');await saved();
      await $('button*=Prefer left').waitForEnabled();await $('button*=Prefer left').click();
      await browser.waitUntil(async()=>(await native('load_session')).screen==='review');await saved();
      const baseline=await native('load_session'),status=await native('session_save_status');
      const processBefore=await supervise('inspect'),context=await native('preview_context');
      assert.equal(status.state,'saved');assert.ok(Number.isSafeInteger(status.durableRevision));
      assert.equal(baseline.review.order.length,2);
      plan=await native('plan_operation',{request:{kind:'rename',imageIds:baseline.review.order,ranks:[1,2],reviewCount:2,naming:'numeric',template:'{rank}.{ext}',start:1,padding:3,destination:null,conflict:'suffix',manifest:false,verifyCopy:false}});
      assert.equal(plan.status,'planned');assert.equal(plan.rows.length,2);
      assert.ok(plan.rows.every(row=>!row.noop&&!row.issue&&windowsLocationKey(row.from)!==windowsLocationKey(row.to)));
      record.baseline={status,context,process:processBefore};record.plan=plan;

      // Real public save failures leave both a pending event and a newer native
      // proposal. Their observed native IDs are independently compared against
      // the eventual copy and synced journal.
      fs.writeFileSync(appendMarker,'image-ranker-e2e-injected-append-failure\n',{flag:'wx'});
      const first=structuredClone(baseline);first.name='native pending event';
      await assert.rejects(()=>native('save_session',{value:first}),/injected desktop session append failure/);
      const firstStatus=await native('session_save_status');
      const second=structuredClone(baseline);second.name='native pending proposal';
      await assert.rejects(()=>native('save_session',{value:second}),/injected desktop session append failure/);
      const secondStatus=await native('session_save_status');
      assert.equal(secondStatus.durableRevision,status.durableRevision);
      assert.notEqual(secondStatus.pendingEventId,firstStatus.pendingEventId);
      assert.equal((await native('load_session')).name,second.name);
      fs.unlinkSync(appendMarker);
      await $('[aria-label="Settings"]').click();
      publish(panicRequest,{nonce,operationId:plan.id});
      publish(barrierRequest,{nonce:gate,label:'files-committed',operationId:plan.id});
      await start('commit_operation',{id:plan.id,contextId:context},'restartOperation');
      operationStarted=true;
      await browser.waitUntil(()=>fs.existsSync(readyPath),{timeout:25000,timeoutMsg:'The real rename did not reach its durable files-committed boundary'});
      const held=JSON.parse(fs.readFileSync(readyPath,'utf8'));
      assert.equal(held.operationId,plan.id);assert.equal(held.status,'files-committed');assert.equal(held.pid,processBefore.processId);
      const moved=imageEvidence(plan);record.held={held,moved};

      // This is an actual React edit while native commit owns mutation. Its save
      // is denied, so the UI state exists only in the renderer until reviewed.
      const rendererName='renderer-only retained after native panic';
      const name=await $('//label[contains(.,"Collection name")]/input');
      await name.setValue(rendererName);
      await browser.waitUntil(async()=>(await $('.save-status').getText()).includes('Save failed'));
      assert.equal(await name.getValue(),rendererName);
      assert.equal((await native('load_session')).name,second.name);
      const renderer=structuredClone(second);renderer.name=rendererName;
      release();
      const result=await outcome('restartOperation');operationStarted=false;
      assert.equal(result.state,'rejected');assert.match(result.error,/FILE_OPERATION_RESTART_REQUIRED/);
      const panicPath=path.join(root,`session-panic-${nonce}.json`);
      await browser.waitUntil(()=>fs.existsSync(panicPath));
      const panic=JSON.parse(fs.readFileSync(panicPath,'utf8'));
      assert.equal(panic.nonce,nonce);assert.equal(panic.operationId,plan.id);assert.equal(panic.pid,processBefore.processId);
      assert.equal(panic.boundary,'after-session-sync-before-ack');
      assert.equal(fs.existsSync(panicRequest),false,'The one-shot request must be consumed before thread-pool reuse');
      const journal=readJournal(panic.journalPath);assert.equal(journal.bytes,panic.journalBytes);
      assert.equal(journal.events.length,status.durableRevision+1);
      assert.deepEqual(imageEvidence(plan),moved);
      const earlyReview=await native('prepare_operation_restart',{expectedCurrentSession:renderer});
      assert.equal(earlyReview.savedRevision,status.durableRevision);assert.equal(earlyReview.pendingActions,3);assert.equal(earlyReview.rendererProposal,true);
      await assert.rejects(()=>native('confirm_operation_restart',{token:earlyReview.token,expectedCurrentSession:renderer}),/OPERATION_RESTART_COPY_REQUIRED/);
      await native('cancel_operation_restart',{token:earlyReview.token});
      record.failure={firstStatus,secondStatus,result,panic,journal:{...journal,events:journal.events.map(event=>({sequence:event.sequence,eventId:event.eventId}))},earlyReview};

      const reloaded=await reloadRenderer(context);const boot=await restartIssue();
      assert.equal((await supervise('inspect',processBefore.processId)).running,true);
      assert.deepEqual(imageEvidence(plan),moved);
      await assert.rejects(()=>native('save_session',{value:renderer}),/FILE_OPERATION_RESTART_REQUIRED|IMAGE_WORK_BUSY/);
      await assert.rejects(()=>native('export_session_copy',{value:renderer}),/FILE_OPERATION_RESTART_REQUIRED|IMAGE_WORK_BUSY/);
      await assert.rejects(()=>native('protection_maintenance_status'),/RECOVERY_BUSY|IMAGE_WORK_BUSY|FILE_OPERATION_RESTART_REQUIRED/);
      record.reload={...reloaded,boot,ordinaryClose:await ordinaryCloseDenied(processBefore.processId)};

      const review=await $('[aria-label="Preserve work before restart"]');await review.waitForDisplayed();
      await review.$('button=Preserve work and review restart').click();
      await review.$('button=Save restart recovery copy').waitForEnabled();
      assert.ok((await review.getText()).includes('Included in this review'));
      assert.equal(await review.$('button=Close for restart').isEnabled(),false);
      await review.$('button=Cancel restart review').click();
      await review.$('button=Preserve work and review restart').waitForEnabled();
      await restartIssue();assert.equal((await supervise('inspect',processBefore.processId)).running,true);
      await review.$('button=Preserve work and review restart').click();
      await review.$('button=Save restart recovery copy').waitForEnabled();
      await review.$('button=Save restart recovery copy').click();
      await browser.waitUntil(async()=>(await review.getText()).includes('Native recovery-copy verification is complete for this restart review.'));
      assert.ok((await review.getText()).includes('Verified for this exact review'));
      await review.$('button=Close for restart').waitForEnabled();
      const wrapper=JSON.parse(fs.readFileSync(copyPath,'utf8')),copy=wrapper.payload;
      assert.equal(wrapper.formatVersion,1);assert.equal(copy.schemaVersion,3);
      assert.equal(copy.history.revision,status.durableRevision);assert.equal(copy.history.events.length,status.durableRevision);
      assert.equal(copy.pendingEvent.target.ui.name,first.name);
      assert.equal(copy.pendingProposal.id,secondStatus.pendingEventId);assert.equal(copy.pendingProposal.target.ui.name,second.name);
      assert.equal(copy.snapshot.ui.name,second.name);assert.equal(copy.durableSnapshot.ui.name,baseline.name);
      assert.equal(copy.rendererProposal.origin,'renderer-only');assert.equal(copy.rendererProposal.targetUi.name,rendererName);
      assert.notEqual(copy.rendererProposal.id,copy.pendingProposal.id);
      assert.match(copy.rendererProposal.nativeBaseDigest,/^[a-f0-9]{64}$/);
      assert.equal(copy.operationEvidence,'retained-native-snapshot');assert.equal(copy.requiresSourceReconnect,true);
      assert.equal(copy.unverifiedFileOutcome.state,'unverified');assert.equal(copy.unverifiedFileOutcome.requestId,earlyReview.requestId);
      assert.equal(copy.operations.find(operation=>operation.id===plan.id).status,'planned','Copy must identify its pre-action evidence, not certify current file outcome');
      const eventId=copy.pendingEvent.event.id;
      assert.equal(eventId,firstStatus.pendingEventId);
      assert.equal(journal.events.filter(event=>event.eventId===eventId).length,1,'The synced-but-unacknowledged event must exist exactly once');
      assert.equal(copy.history.events.some(event=>event.eventId===eventId),false,'The immutable saved history must not falsely acknowledge the panicking append');
      const copyHash=digest(copyPath);record.copy={path:copyPath,sha256:copyHash,payload:copy};
      assert.deepEqual(imageEvidence(plan),moved);
      await browser.saveScreenshot(path.join(root,'operation-restart-verified-copy.png'));
      let closeResponseError=null;
      try{await review.$('button=Close for restart').click();}catch(error){closeResponseError=String(error);}
      let exited;
      await browser.waitUntil(async()=>{exited=await supervise('inspect',processBefore.processId);return !exited.running;},{timeout:20000,timeoutMsg:'Verified restart close must end the exact supervised native process'});
      record.close={closeResponseError,exited};
      assert.equal(digest(copyPath),copyHash);assert.deepEqual(imageEvidence(plan),moved);

      // A new native process, not a renderer refresh. Startup may reconcile
      // catalog metadata; it must not repeat, undo or duplicate image moves.
      await browser.reloadSession();await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();
      const processAfter=await supervise('inspect');assert.notEqual(processAfter.processId,processBefore.processId);
      await browser.waitUntil(async()=>{
        const boot=await native('get_boot_status');
        return boot.trustedOperationsAvailable&&(await native('list_operations')).some(operation=>operation.id===plan.id);
      });
      const after=await native('get_boot_status'),operations=await native('list_operations');
      assert.equal(after.issues.some(issue=>issue.code==='FILE_OPERATION_RESTART_REQUIRED'),false);
      const retained=operations.filter(operation=>operation.id===plan.id);assert.equal(retained.length,1);
      assert.equal(retained[0].order,plan.order);assert.ok(['files-committed','artifact-repair-only','complete'].includes(retained[0].status));
      assert.deepEqual(retained[0].rows.map(row=>({from:row.from,to:row.to,sourceIdentity:row.sourceIdentity})),plan.rows.map(row=>({from:row.from,to:row.to,sourceIdentity:row.sourceIdentity})));
      assert.deepEqual(imageEvidence(plan),moved);assert.equal(digest(copyPath),copyHash);
      const recoveredJournal=readJournal(panic.journalPath);
      assert.equal(recoveredJournal.events.filter(event=>event.eventId===eventId).length,1);
      record.reopened={process:processAfter,boot:after,operation:retained[0],journalSha256:recoveredJournal.sha256};
    } catch(error) {primaryFailure=error;record.failureMessage={message:String(error),stack:error.stack};throw error;}
    finally {
      const cleanup=[];
      const attempt=(name,action)=>{try{action();}catch(error){cleanup.push({name,error:String(error)});}};
      attempt('release exact committed boundary',()=>{if(fs.existsSync(readyPath))release();});
      if(primaryFailure&&operationStarted) {
        try {record.cleanupOperation=await outcome('restartOperation');}
        catch(error){cleanup.push({name:'observe admitted operation retirement',error:String(error)});}
      }
      for(const [file,expected] of [[panicRequest,nonce],[barrierRequest,gate]])attempt(`remove ${path.basename(file)}`,()=>{
        if(!fs.existsSync(file))return;assert.equal(JSON.parse(fs.readFileSync(file,'utf8')).nonce,expected);fs.unlinkSync(file);
      });
      attempt('remove own append fault',()=>{if(fs.existsSync(appendMarker))fs.unlinkSync(appendMarker);});
      record.cleanup={gateReleased,errors:cleanup};
      fs.writeFileSync(path.join(root,'operation-restart-evidence.json'),JSON.stringify(record,null,2));
      if(cleanup.length&&!primaryFailure)throw new AggregateError(cleanup.map(item=>new Error(`${item.name}: ${item.error}`)),'Restart fixture teardown failed');
    }
  });
});
