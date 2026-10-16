import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {randomUUID} from 'node:crypto';
import {digest} from './fixtures.mjs';
import {assertWindowsLocation} from './windows-paths.mjs';

const root = process.env.IMAGE_RANKER_E2E_ROOT;
const inventory = JSON.parse(fs.readFileSync(path.join(root, 'inventory.json'), 'utf8'));
const byOriginal = new Map(inventory.map(row => [path.basename(row.path), row]));
const button = text => $(`button=${text}`);
// The test configuration exposes the public Tauri global. Call it through the
// current WebDriver session: the service caches a provider under the old session
// ID and otherwise falls back to its embedded-driver endpoint after reloadSession.
async function native(command, args = {}) {
  const {result}=await browser.executeAsync((cmd,input,done)=>{
    void (async()=>{
      const bootstrap=['get_boot_status','preview_context','app_info','license_notices'];
      // Test probes acquire the current native context. Explicit contexts are
      // evidence inputs, including stale ones, and must never be overwritten.
      if(!bootstrap.includes(cmd)&&!Object.prototype.hasOwnProperty.call(input,'contextId')) {
        input={...input,contextId:await window.__TAURI__.core.invoke('preview_context')};
      }
      return window.__TAURI__.core.invoke(cmd,input);
    })().then(value=>done({result:{ok:true,value}}),error=>done({result:{ok:false,error:String(error)}}));
  },command,args);
  if(!result.ok) throw new Error(`${command}: ${result.error}`);
  return result.value;
}
const saved = () => browser.waitUntil(async () => (await $('.save-status').getText()).includes('Saved locally'), {timeoutMsg: 'Expected a durable native save'});
async function click(text) {
  const dialogs=await $$('[role="dialog"]');
  let activeDialog;
  for(const dialog of dialogs) if(await dialog.isDisplayed()) activeDialog=dialog;
  const element=activeDialog?await activeDialog.$(`button=${text}`):await button(text);
  await element.waitForEnabled();await element.click();
}
async function markDocument() {
  const marker=randomUUID();
  await browser.execute(value=>{window.__imageRankerDocumentEvidence=value;},marker);
  return {marker,contextId:await native('preview_context'),process:await supervise('inspect')};
}
async function afterCommittedReload(before) {
  await browser.waitUntil(()=>browser.execute(marker=>window.__imageRankerDocumentEvidence!==marker,before.marker),{timeout:30000,timeoutMsg:'A committed native workspace change must cause a real document reload'});
  await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();await saved();
  const boot=await native('get_boot_status'),loaded=await native('load_session_state');
  assert.notEqual(loaded.contextId,before.contextId,'Reload must acquire a new native renderer context');
  assert.equal(loaded.contextId,boot.contextId);assert.equal(loaded.profileGeneration,boot.profileGeneration);
  assert.equal(loaded.saveStatus.state,'saved');
  const process=await supervise('inspect',before.process.processId);
  assert.equal(process.processId,before.process.processId,'Profile/branch switching must reload the renderer without restarting the native process');
  return {boot,loaded,process};
}
async function prepareProfile(mode,choice) {
  fs.writeFileSync(path.join(root,'profile-choice.txt'),choice);
  await $('[aria-label="Settings"]').click();
  await click(mode==='create'?'Choose recovery storage':'Open existing profile');
}
async function confirmProfile(mode,choice,expectedPath) {
  const before=await markDocument();
  await prepareProfile(mode,choice);
  const preview=await $('[aria-label="Prepared profile preview"]');await preview.waitForDisplayed();
  // FIX 2: Compare the full rendered location, including namespaced native paths.
  assertWindowsLocation(await preview.$('p > strong').getText(),expectedPath,'Review must show the exact picker-granted profile location');
  const approve=await preview.$('input[type="checkbox"]');await approve.click();
  await click('Switch profile');
  const after=await afterCommittedReload(before);
  assertWindowsLocation(after.boot.profile.displayPath,expectedPath);
  return {before,...after};
}
async function supervise(action,expectedPid) {
  if(process.platform!=='win32') throw new Error('A native window/process supervisor is required for this target');
  const args=['-NoProfile','-File',path.resolve('tests/desktop/kill-app.ps1'),'-DriverId',process.env.IMAGE_RANKER_DRIVER_PID,'-ApplicationPath',path.resolve('src-tauri/target/debug/image-ranker.exe'),'-FixtureRoot',root,'-Action',action];
  if(expectedPid!==undefined) args.push('-ExpectedProcessId',String(expectedPid));
  const {stdout}=await promisify(execFile)('powershell.exe',args,{windowsHide:true,timeout:15000});
  const result=JSON.parse(stdout.trim());
  fs.appendFileSync(path.join(root,'process-supervision.jsonl'),JSON.stringify({...result,observedAt:new Date().toISOString()})+'\n');
  return result;
}
async function inspectDecoderProcess(pid) {
  assert.ok(Number.isSafeInteger(pid)&&pid>0);
  const command=`$ErrorActionPreference='Stop'; $worker=Get-CimInstance Win32_Process -Filter 'ProcessId=${pid}'; if($null -eq $worker){'null'}else{$worker | Select-Object ProcessId,ParentProcessId,ExecutablePath,CommandLine | ConvertTo-Json -Compress}`;
  const {stdout}=await promisify(execFile)('powershell.exe',['-NoProfile','-Command',command],{windowsHide:true,timeout:10000});
  return JSON.parse(stdout.trim());
}
async function killAndReopen({expectRecovery=false,expectedPid}={}) {
  if(process.platform!=='win32') throw new Error('A native process-kill adapter is required for this target');
  const killed=await supervise('terminate',expectedPid);
  fs.appendFileSync(path.join(root,'process-restarts.jsonl'),JSON.stringify(killed)+'\n');
  if(expectedPid) assert.equal(killed.terminatedPid,expectedPid,'The supervisor must terminate the exact process that reached the barrier');
  await browser.reloadSession();
  await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();
  if(expectRecovery) await browser.waitUntil(async()=>(await native('get_boot_status')).mode==='recovery');
  else await saved();
}
async function vote(result = 'left') {
  const element = await $(result === 'both' ? 'button.vote.both' : `button*=Prefer ${result}`);
  await element.waitForEnabled();
  const before = (await native('load_session')).run.history.length;
  await element.click();
  await browser.waitUntil(async () => (await native('load_session')).run.history.length === before + 1, {timeoutMsg: 'A vote must persist exactly once'});
  await saved();
}

async function copyFromReview({scope='picks',count=3,policy='suffix',naming='numeric',verify=true}={}) {
  const prior=new Set((await native('list_operations')).map(operation=>operation.id));
  await click(scope==='all'?'Create ranked copies':`Export ${count} picks`);
  await $('//label[normalize-space(text())="Naming"]/select').selectByAttribute('value',naming);
  await click('Browse');
  await $('//label[contains(.,"If a file already exists")]/select').selectByAttribute('value',policy);
  if(verify) await $('//label[contains(.,"Verify copied bytes")]/input').click();
  await click('Build preview');
  await $('[role="dialog"] .confirm input').waitForDisplayed();
  await $('[role="dialog"] .confirm input').click();
  await click(`Copy ${count} images`);
  await browser.waitUntil(async()=>(await native('list_operations')).some(operation=>!prior.has(operation.id)&&operation.kind==='copy'&&operation.status==='complete'),{timeoutMsg:'Expected a completed new copy operation'});
  const operation=(await native('list_operations')).find(operation=>!prior.has(operation.id)&&operation.kind==='copy'&&operation.status==='complete');
  await click('Close');
  for(const original of inventory) assert.equal(digest(original.path),original.hash,'Copy must leave every original unchanged');
  return operation;
}

async function startMode(mode,count) {
  await $('nav[aria-label="Workflow"]').$('button*=Sources').click();await saved();
  await click('None');await saved();
  for(let i=1;i<=count;i++) { await $(`input[aria-label="Select fixture-${String(i).padStart(2,'0')}.bmp"]`).click();await saved(); }
  await $('[aria-label="Ranking mode"]').selectByAttribute('value',mode);await saved();
  await click(count===1?'Review 1 image':`Rank ${count} images`);
  await click(count===1?'Open review':'Start comparing');await saved();
}

// FIX 1: These checks use the real public bridge and rendered text. A status
// surface being available is not evidence of a held or cancellable worker.
function assertRecoveryWorkStatus(status) {
  assert.ok(Number.isSafeInteger(status.limit)&&status.limit>0);
  assert.ok(Array.isArray(status.recent)&&status.recent.length<=16);
  for(const job of [...(status.active?[status.active]:[]),...status.recent]) {
    for(const field of ['id','contextId','profileGeneration','phase']) assert.ok(typeof job[field]==='string'&&job[field].length>0,`Recovery work ${field} must be native data`);
    assert.ok(['storage-status','prepare-resume','preview-cleanup','cleanup','inspect-copy','restore-copy',
      'startup-load','prepare-profile','switch-profile','registry-status','preview-registry-maintenance',
      'registry-maintenance'].includes(job.kind),`Unknown native recovery-work kind: ${job.kind}`);
    assert.ok(['running','cancelling','completed','cancelled','failed'].includes(job.state));
    assert.ok(Number.isSafeInteger(job.completed)&&job.completed>=0);
    assert.ok(job.total===null||(Number.isSafeInteger(job.total)&&job.total>=job.completed));
    assert.equal(typeof job.cancelRequested,'boolean');
    assert.ok(job.message===null||typeof job.message==='string');
  }
  if(status.active) assert.ok(['running','cancelling'].includes(status.active.state));
  for(const job of status.recent) assert.ok(['completed','cancelled','failed'].includes(job.state));
}
async function checkRecoveryWorkSurface(operationId) {
  // Shell-first startup exposes status before history has been installed. The
  // observational comparison starts only after that real startup owner exits.
  await browser.waitUntil(async()=>{
    const boot=await native('get_boot_status'),work=await native('recovery_work_status');
    assertRecoveryWorkStatus(work);
    return boot.state!=='loading'&&!work.active;
  },{timeoutMsg:'Startup history loading must finish before comparing retained operation records'});
  const operations=await native('list_operations');
  const controls=await $('[aria-label="Recovery work controls"]');await controls.waitForDisplayed();
  const before=await native('recovery_work_status');assertRecoveryWorkStatus(before);
  const refresh=await controls.$('button=Refresh recovery work status');await refresh.waitForEnabled();await refresh.click();
  let after;
  await browser.waitUntil(async()=>{
    after=await native('recovery_work_status');assertRecoveryWorkStatus(after);
    return !after.active&&await refresh.isEnabled()&&(await controls.getText()).includes('No recovery work is active.');
  },{timeoutMsg:'Restricted startup must expose an independently readable recovery-work status and manual refresh'});
  assert.equal(await controls.$('[role="alert"]').isExisting(),false);
  assert.deepEqual(await native('list_operations'),operations,'Reading recovery status must not repair or mutate interrupted operation records');
  fs.appendFileSync(path.join(root,'recovery-work-surface-evidence.jsonl'),JSON.stringify({evidenceClass:'restricted-startup-status-and-manual-refresh-only-no-held-recovery-worker',operationId,before,after,renderedText:await controls.getText()})+'\n');
}
async function checkProtectionSurface() {
  const panel=await $('[aria-label="Protection capacity"]');await panel.waitForDisplayed();
  assert.ok((await panel.getText()).includes('Capacity has not been checked.'),'Help must not silently initiate a protection read');
  assert.equal(await panel.$('[aria-label="Observed protection capacity"]').isExisting(),false);
  const before=await native('protection_status');
  const labels={ready:'Protection capacity available','ordinary-limit':'Ordinary admission limit reached','recovery-limit':'Recovery publication limit reached',unavailable:'Protection capacity unavailable'};
  assert.ok(Object.hasOwn(labels,before.state));
  const check=await panel.$('button=Check protection capacity');await check.scrollIntoView();await check.click();
  const refresh=await panel.$('button=Refresh protection capacity');await refresh.waitForEnabled();
  assert.equal(await panel.$('[role="status"]').getText(),labels[before.state]);
  const after=await native('protection_status');
  assert.deepEqual(after,before,'These read-only checks must observe the same quiescent protection capacity');
  const rendered=await panel.getText();
  if(before.state==='unavailable') {
    assert.equal(await panel.$('[aria-label="Observed protection capacity"]').isExisting(),false,'Unavailable capacity must not be rendered as zero or free space');
    assert.ok(rendered.includes('No current capacity reading is available.'));
    if(before.message) assert.ok(rendered.includes(before.message));
  } else {
    assert.ok(before.capacity,'A usable capacity state must contain actual counters');
    const fields=[['Recorded usage','usedBytes','usedClaims'],['Hard limit','hardBytes','hardClaims'],['Ordinary admission ceiling','ordinaryByteLimit','ordinaryClaimLimit'],['Ordinary headroom','ordinaryBytesRemaining','ordinaryClaimsRemaining'],['Recovery publication headroom','publicationBytesRemaining','publicationClaimsRemaining'],['Reserved completions','reservedCompletionBytes','reservedCompletionClaims'],['Emergency allowance','emergencyByteAllowance','emergencyClaimAllowance'],['Maintenance allowance','maintenanceByteAllowance','maintenanceClaimAllowance']];
    for(const [,byteField,claimField] of fields) for(const field of [byteField,claimField]) assert.ok(Number.isSafeInteger(before.capacity[field])&&before.capacity[field]>=0,`Invalid native capacity field ${field}`);
    const renderedPairs=await browser.execute(()=>Object.fromEntries([...document.querySelectorAll('[aria-label="Observed protection capacity"] dt')].map(term=>[term.textContent,term.nextElementSibling?.textContent])));
    const expectedPairs=await browser.execute((rows,capacity)=>Object.fromEntries(rows.map(([label,b,c])=>[label,`${capacity[b].toLocaleString()} bytes · ${capacity[c].toLocaleString()} claims`])),fields,before.capacity);
    assert.deepEqual(renderedPairs,expectedPairs,'Every displayed count must match its native usage/headroom/reservation field');
    assert.ok(rendered.includes('Observed headroom does not guarantee that the next operation fits.'));
  }
  assert.ok(rendered.includes('Protection storage maintenance can compact records while preserving historical protections.'));
  await $('button=Review protection storage maintenance').waitForEnabled();
  fs.writeFileSync(path.join(root,'protection-capacity-evidence.json'),JSON.stringify({evidenceClass:'manual-native-protection-capacity-read-and-rendered-field-mapping',before,after,renderedText:rendered},null,2));
}

// FIX 2: Clear completion is scoped to an exact native request/generation.
// Observational reads never start another inventory or infer success from idle.
function assertClearReceipt(receipt) {
  assert.ok(receipt&&typeof receipt.requestId==='string'&&receipt.requestId.length>0,'Clear must have a native request identity');
  assert.match(receipt.generation,/^(0|[1-9][0-9]*)$/,'Cache generation must be a lossless decimal string');
  assert.equal(typeof receipt.generation,'string');
  assert.ok(['pending','running','waiting-for-pins','completed','completed-with-retained-items','cancelled','failed'].includes(receipt.state));
  for(const field of ['retainedFiles','retainedBytes']) assert.ok(Number.isSafeInteger(receipt[field])&&receipt[field]>=0);
  assert.equal(typeof receipt.retainedCountsKnown,'boolean');
  assert.ok(Array.isArray(receipt.reasons)&&receipt.reasons.every(reason=>typeof reason==='string'));
  assert.ok(receipt.retryOf===null||typeof receipt.retryOf==='string');
}
async function waitForClear(identity,state,condition=()=>true) {
  let snapshot;
  await browser.waitUntil(async()=>{
    snapshot=await native('cache_info',{refresh:false,requestId:identity.requestId});
    assertClearReceipt(snapshot.clearRequest);
    assert.equal(snapshot.clearRequest.requestId,identity.requestId);
    assert.equal(snapshot.clearRequest.generation,identity.generation);
    return snapshot.clearRequest.state===state&&condition(snapshot);
  },{timeout:15000,timeoutMsg:`Exact clear request ${identity.requestId} must reach ${state}`});
  return snapshot;
}
async function assertWaitingCacheSurface(pinnedFiles) {
  const section=await $('.cache-settings');
  await browser.waitUntil(async()=>(await section.getText()).includes('Clear request is waiting for displayed previews or retained resources to be released.'),{timeoutMsg:'Settings must show the exact pending clear instead of a successful global deletion'});
  const text=await section.getText();
  assert.equal(await section.$('.success').isExisting(),false);
  assert.equal(text.includes('Unused cache entries cleared.'),false);
  assert.ok(text.includes(`${pinnedFiles} displayed or prefetched previews (`));
  assert.ok((await section.$('[aria-label="Clear request details"]').getText()).includes(`${pinnedFiles} retained cache files (`));
  return text;
}

describe('Real native image workflow', () => {
  before(async () => {
    await browser.waitUntil(() => browser.execute(() => typeof window.__TAURI__?.core?.invoke === 'function'), {timeoutMsg:'Expected the native command bridge after application startup'});
    await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();
  });
  it('contains keyboard focus in dialogs and returns it to the opener', async () => {
    const opener=await $('[aria-label="Help and keyboard shortcuts"]');await opener.waitForEnabled();await opener.click();
    assert.equal(await browser.execute(()=>document.activeElement?.getAttribute('aria-label')),'Close dialog');
    await browser.keys(['\uE008','\uE004','\uE000']);
    const focusIsLast=await browser.execute(()=>{const d=document.querySelector('[role="dialog"]');const list=[...d.querySelectorAll('button:not([disabled]),input:not([disabled]),select:not([disabled]),a[href],[tabindex="0"]')].filter(el=>el.getClientRects().length);return document.activeElement===list.at(-1);});
    assert.equal(focusIsLast,true);
    await browser.keys('\uE004');assert.equal(await browser.execute(()=>document.activeElement?.getAttribute('aria-label')),'Close dialog');
    await browser.keys('\uE00C');
    assert.equal(await browser.execute(()=>document.activeElement?.getAttribute('aria-label')),'Help and keyboard shortcuts');
  });
  it('scans overlapping roots, decodes previews and preserves unsupported files', async () => {
    await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();
    await $('[type="checkbox"]').click(); // Include subfolders, the only initial checkbox.
    await saved();
    await click('Add folder');
    await browser.waitUntil(async () => (await native('load_session'))?.report?.images?.length === 20);
    await saved();
    const session = await native('load_session');
    assert.equal(session.report.sources.length, 2);
    assert.equal(new Set(session.report.images.map(image => image.id)).size, 20);
    assert.equal(session.selected.length, 20);
    assert.ok(session.report.ignored.some(entry => entry.path.endsWith('corrupt.bmp')));
    for (const image of session.report.images) {
      assert.equal(image.width, 96);
      assert.equal(image.height, 64);
      assert.ok(fs.existsSync(image.previewPath));
      assert.equal(digest(image.path), byOriginal.get(image.originalName).hash);
    }
    assert.equal(fs.readFileSync(path.join(root, 'source/notes.txt'), 'utf8'), 'An unsupported file must remain unchanged.');
    await browser.saveScreenshot(path.join(root, 'sources.png'));
  });

  it('ranks 15 candidates, undoes exactly, and restores a persisted comparison', async () => {
    for (let i = 1; i <= 5; i++) {
      await $(`input[aria-label="Select fixture-${String(i).padStart(2, '0')}.bmp"]`).click();
      await saved();
    }
    await $('[aria-label="Ranking mode"]').selectByAttribute('value', 'full-pairwise');
    await saved();
    await click('Rank 15 images');
    await click('Start comparing');
    await saved();
    const original = (await native('load_session')).run;
    await vote('left'); await vote('both'); await vote('right');
    for (let i = 0; i < 3; i++) { await $('button*=Undo').click(); await saved(); }
    const restored = (await native('load_session')).run;
    assert.equal(restored.history.length, 0);
    assert.equal(restored.pair.left, original.pair.left);
    assert.equal(restored.pair.right, original.pair.right);
    assert.notEqual(restored.pair.id, original.pair.id, 'Undo must invalidate stale pair tokens');
    await vote('both');
    const beforeReload = (await native('load_session')).run;
    await browser.refresh(); // Renderer restart; process-kill recovery has separate native fault tests.
    await saved();
    assert.deepEqual((await native('load_session')).run, beforeReload);
    await killAndReopen();
    assert.deepEqual((await native('load_session')).run,beforeReload,'Process termination must retain the acknowledged exact pair/history');
    let count = 1;
    while ((await native('load_session')).run.pair) {
      await vote(count % 11 === 0 ? 'both' : count % 2 === 0 ? 'right' : 'left');
      if (++count > 105) throw new Error('Full pairwise exceeded its exact pair count');
    }
    await browser.waitUntil(async () => (await native('load_session')).screen === 'review');
    const session = await native('load_session');
    assert.equal(session.run.history.length, 105);
    assert.equal(session.review.order.length, 15);
    assert.equal(new Set(session.review.order).size, 15);
    await browser.saveScreenshot(path.join(root, 'review.png'));
  });

  it('moves rank nine to rank two and preserves the manual order across restart', async () => {
    const before=(await native('load_session')).review.order;
    await $('button[aria-label^="Select rank 9:"]').scrollIntoView();await $('button[aria-label^="Select rank 9:"]').click();
    await click('Move 1 to rank…');await $('[role="dialog"] input[type="number"]').setValue('2');await click('Move images');await saved();
    const expected=[...before];const [moved]=expected.splice(8,1);expected.splice(1,0,moved);
    assert.deepEqual((await native('load_session')).review.order,expected);
    await killAndReopen();assert.deepEqual((await native('load_session')).review.order,expected);
  });

  it('previews and commits real rename, verifies bytes, then restores all original paths', async () => {
    const before = await native('load_session');
    const selected = before.review.order.map(id => before.report.images.find(image => image.id === id));
    await click('Rename originals');
    await click('Build preview');
    await $('[role="dialog"] .confirm input').waitForDisplayed();
    const heldImage=selected[13],requestId=randomUUID(),nonce=randomUUID();
    const contextId=await native('preview_context'),process=await supervise('inspect');
    const holdFile=path.join(root,'image-worker-hold.json');
    const evidenceFile=path.join(root,'rename-preview-drain-evidence.json');
    const request={id:heldImage.id,size:384,expectedContentRevision:heldImage.contentRevision,requestId,contextId};
    let operation;
    try {
      fs.writeFileSync(holdFile,JSON.stringify({requestId,nonce,stage:'decode'}),{flag:'wx'});
      await browser.execute(input=>{
        window.__renameHeldPreview={state:'pending'};
        void window.__TAURI__.core.invoke('request_preview',input).then(
          value=>{window.__renameHeldPreview={state:'resolved',value};},
          error=>{window.__renameHeldPreview={state:'rejected',error:String(error)};},
        );
      },request);
      let held;
      await browser.waitUntil(async()=>{
        held=await native('image_work_status');
        return held.decoder.stage===`fixture-held:${nonce}:decode`;
      },{timeout:15000,timeoutMsg:'Rename must be challenged by an actual held decoder source reader'});
      assert.equal(fs.existsSync(holdFile),false);
      const worker=await inspectDecoderProcess(held.decoder.workerPid);
      assert.ok(worker);assert.equal(worker.ParentProcessId,process.processId);
      assertWindowsLocation(path.resolve(worker.ExecutablePath).toLowerCase(),path.resolve('src-tauri/target/debug/image-ranker.exe').toLowerCase());
      assert.ok(worker.CommandLine.includes('--image-ranker-decode-worker='));
      fs.writeFileSync(evidenceFile,JSON.stringify({stage:'actual-child-held-before-commit',request,nonce,held,worker},null,2));
      await $('[role="dialog"] .confirm input').click();
      await click('Rename 15 images');
      await browser.waitUntil(async () => (await native('list_operations')).some(op => op.kind === 'rename' && op.status === 'complete'));
      operation = (await native('list_operations')).find(op => op.kind === 'rename' && op.status === 'complete');
      assert.equal(await inspectDecoderProcess(worker.ProcessId),null,'Successful rename requires actual held helper exit');
      await browser.waitUntil(()=>browser.execute(()=>window.__renameHeldPreview?.state!=='pending'));
      // WebDriver reserves a top-level error field for protocol failures.
      const {result}=await browser.execute(()=>({result:window.__renameHeldPreview}));
      assert.equal(result.state,'rejected');assert.match(result.error,/IMAGE_JOB_CANCELLED|DECODE_CANCELLED/);
      fs.writeFileSync(evidenceFile,JSON.stringify({stage:'rename-committed-after-child-exit',request,nonce,held,worker,result,operation},null,2));
    } finally {
      if(fs.existsSync(holdFile))fs.unlinkSync(holdFile);
      await native('cancel_image_work',{requestId});
      const {result}=await browser.execute(()=>({result:window.__renameHeldPreview}));
      if(result?.state==='resolved'&&result.value?.leaseId)await native('release_preview',{leaseId:result.value.leaseId});
    }
    assert.equal(operation.rows.length, 15);
    for (const row of operation.rows) assert.equal(digest(row.to), byOriginal.get(selected.find(image => image.id === row.imageId).originalName).hash);
    assert.ok(fs.existsSync(operation.manifestPath));
    await click('Close');
    await $('[aria-label="File operation history"]').click();
    await click('Undo file operation');
    await click('Confirm undo');
    await browser.waitUntil(async () => (await native('list_operations')).find(op => op.id === operation.id).status === 'rolled-back');
    for (const image of inventory) assert.equal(digest(image.path), image.hash);
    await click('Close');
  });

  it('exports sparse global ranks and preserves bytes across suffix, skip and overwrite policies', async () => {
    const session=await native('load_session');
    const pickedRanks=[2,9,15];
    for(const rank of pickedRanks) {
      const image=session.report.images.find(image=>image.id===session.review.order[rank-1]);
      await $('[aria-label="Search ranked images"]').setValue(image.originalName);
      await $(`button[aria-label="Add final pick: ${image.originalName}"]`).click();await saved();
    }
    await $('[aria-label="Search ranked images"]').setValue('');
    const first=await copyFromReview();
    assert.deepEqual(first.rows.map(row=>row.rank),pickedRanks);
    for(const row of first.rows) {
      assert.equal(path.basename(row.to),`${String(row.rank).padStart(3,'0')}.bmp`);
      assert.equal(row.verification,'sha256-readback');assert.equal(digest(row.to),row.hash);
    }
    const manifest=JSON.parse(fs.readFileSync(first.manifestPath,'utf8'));
    assert.equal(manifest.files.length,3);
    const suffixed=await copyFromReview();
    for(const row of suffixed.rows) { assert.ok(path.basename(row.to).includes(' (2).bmp'));assert.equal(digest(row.to),row.hash); }
    const skipped=await copyFromReview({policy:'skip'});
    assert.ok(skipped.rows.every(row=>row.step==='skipped'));
    for(const row of first.rows) assert.equal(digest(row.to),row.hash,'Skip must preserve existing outputs');
    const beforeOverwrite=new Map(first.rows.map((row,index)=>{
      const bytes=Buffer.from(`Independent destination contents ${index}`);fs.writeFileSync(row.to,bytes);return [row.to,bytes];
    }));
    const replaced=await copyFromReview({policy:'overwrite'});
    for(const row of replaced.rows) {assert.equal(digest(row.to),row.hash);assert.deepEqual(fs.readFileSync(row.backup),beforeOverwrite.get(row.to));}
    await native('undo_operation',{id:replaced.id});
    for(const [file,bytes] of beforeOverwrite) assert.deepEqual(fs.readFileSync(file),bytes,'Undo must restore the exact preexisting destination bytes');
    await browser.refresh();await saved();
  });

  it('creates ranked copies of the complete review independently of final picks', async () => {
    const operation=await copyFromReview({scope:'all',count:15,naming:'prefix',verify:false});
    assert.equal(operation.rows.length,15);
    assert.deepEqual(operation.rows.map(row=>row.rank),Array.from({length:15},(_,i)=>i+1));
    for(const row of operation.rows) { assert.equal(digest(row.to),row.hash);assert.equal(row.verification,'streamed-sha256'); }
  });

  for(const mode of ['winner-stays','adaptive']) it(`completes ${mode} with exact undo and process-restart continuity`, async()=>{
    await startMode(mode,6);const initial=(await native('load_session')).run;
    await vote('both');await vote('left');await vote('right');
    for(let i=0;i<3;i++){await $('button*=Undo').click();await saved();}
    const undone=(await native('load_session')).run;
    assert.equal(undone.history.length,0);assert.equal(undone.pair.left,initial.pair.left);assert.equal(undone.pair.right,initial.pair.right);
    await vote('both');const savedRun=(await native('load_session')).run;
    await killAndReopen();assert.deepEqual((await native('load_session')).run,savedRun);
    let decisions=1;while((await native('load_session')).run.pair){await vote(decisions%3===0?'both':decisions%2?'left':'right');if(++decisions>50)throw new Error(`${mode} did not terminate`);}
    const done=await native('load_session');assert.equal(done.review.order.length,6);assert.equal(new Set(done.review.order).size,6);
  });

  it('exports a single image through the explicit review bypass', async()=>{
    await startMode('winner-stays',1);const session=await native('load_session');
    assert.equal(session.run.history.length,0);assert.equal(session.review.order.length,1);
    const operation=await copyFromReview({scope:'all',count:1,naming:'prefix'});
    assert.equal(operation.rows.length,1);assert.equal(digest(operation.rows[0].to),operation.rows[0].hash);
  });

  for(const kind of ['rename','copy']) for(const action of ['resume','rollback'])
    it(`${action}s a ${kind} interrupted after a real native move through startup recovery`,async()=>{
      const earlier=new Set((await native('list_operations')).map(operation=>operation.id));
      const occupant=path.join(root,'output','001.bmp');
      const occupantBytes=Buffer.from(`Existing output retained for ${action}`);
      if(kind==='copy') fs.writeFileSync(occupant,occupantBytes);
      await click(kind==='rename'?'Rename originals':'Create ranked copies');
      if(kind==='copy') { await click('Browse');await $('//label[contains(.,"If a file already exists")]/select').selectByAttribute('value','overwrite'); }
      await click('Build preview');
      await $('[role="dialog"] .confirm input').waitForDisplayed();
      const plan=(await native('list_operations')).find(operation=>!earlier.has(operation.id)&&operation.status==='planned');
      assert.ok(plan,'Planning must durably identify the operation before a file move');
      const nonce=randomUUID(),label=kind==='rename'?'rename-stage-moved':'copy-backup-moved';
      const readyPath=path.join(root,`barrier-${nonce}.ready.json`);
      fs.writeFileSync(path.join(root,'fault-request.json'),JSON.stringify({nonce,label,operationId:plan.id}));
      await $('[role="dialog"] .confirm input').click();await click(`${kind==='rename'?'Rename':'Copy'} 1 images`);
      let reached;
      await browser.waitUntil(()=>{
        try {reached=JSON.parse(fs.readFileSync(readyPath,'utf8'));return reached.nonce===nonce&&reached.operationId===plan.id;} catch {return false;}
      },{timeout:20000,timeoutMsg:`Native barrier ${label} was not reached`});
      await killAndReopen({expectRecovery:true,expectedPid:reached.pid});
      assert.equal(fs.existsSync(path.join(root,`barrier-${nonce}.timed-out`)),false);
      await checkRecoveryWorkSurface(plan.id);
      await click('Recover file operations');
      await click(action==='resume'?'Resume operation':'Roll back safely');await click(`Confirm ${action}`);
      await browser.waitUntil(async()=>(await native('get_boot_status')).mode==='ready');await saved();
      const finished=(await native('list_operations')).find(operation=>operation.id===plan.id);
      assert.equal(finished.status,action==='resume'?'complete':'rolled-back');
      if(action==='resume') {
        for(const row of finished.rows) assert.equal(digest(row.to),row.hash);
        await native('undo_operation',{id:finished.id});await browser.refresh();await saved();
      }
      if(kind==='copy') assert.deepEqual(fs.readFileSync(occupant),occupantBytes);
      for(const original of inventory) assert.equal(digest(original.path),original.hash);
    });

  it('reopens a session recovery copy with retained history and explicit source reconnection',async()=>{
    const before=await native('load_session');
    await $('[aria-label="Settings"]').click();await click('Save recovery copy');
    const copyPath=path.join(root,'session-recovery.json');
    await browser.waitUntil(()=>fs.existsSync(copyPath)&&fs.statSync(copyPath).size>100);
    const copyHash=digest(copyPath);
    await click('Open recovery copy');await click('Open recovery copy');
    const beforeRestore=await markDocument();
    await $('button=Restore as a new session').waitForEnabled();await click('Restore as a new session');
    await afterCommittedReload(beforeRestore);
    const restored=await native('load_session');
    assert.deepEqual(restored.run,before.run);assert.deepEqual(restored.review,before.review);
    assert.equal(restored.archivedRuns.length,before.archivedRuns.length);
    assert.ok(restored.report.sources.every(source=>!source.available),'Imported paths must not grant folder access');
    for(const source of restored.report.sources) {
      fs.writeFileSync(path.join(root,'source-choice.txt'),path.basename(source.path)==='nested'?'nested':'source');
      await native('reconnect_source',{id:source.id});
    }
    await browser.refresh();await saved();
    const reconnected=await native('load_session');
    assert.ok(reconnected.report.sources.every(source=>source.available));
    assert.ok(reconnected.report.images.every(image=>image.availability==='ready'));
    assert.deepEqual(reconnected.run,before.run);assert.equal(digest(copyPath),copyHash,'Import must preserve its source bundle');
    await killAndReopen();assert.deepEqual((await native('load_session')).run,before.run);
  });

  it('cancels both native profile pickers without changing the active workspace',async()=>{
    await saved();
    const baseline=await native('load_session'),boot=await native('get_boot_status');
    const marker=await markDocument();
    const untouched=fs.readdirSync(path.join(root,'recovery-profile'));
    for(const mode of ['create','open']) {
      await prepareProfile(mode,'cancel');
      await browser.waitUntil(async()=>!(await $('button=Keep current profile').isExisting())&&await browser.execute(()=>document.querySelector('.app-shell')?.inert===false),{timeoutMsg:'Cancelled native picker must return to the unchanged Settings dialog'});
      await $('button=Done').waitForEnabled();await click('Done');
      assert.equal(await browser.execute(value=>window.__imageRankerDocumentEvidence===value,marker.marker),true,'Picker cancellation must not reload the document');
      assert.deepEqual(await native('load_session'),baseline);
      assert.deepEqual((await native('get_boot_status')).profile,boot.profile);
      assert.equal(await native('preview_context'),marker.contextId);
    }
    assert.deepEqual(fs.readdirSync(path.join(root,'recovery-profile')),untouched);
    for(const original of inventory) assert.equal(digest(original.path),original.hash);
  });

  it('rejects an occupied recovery folder and preserves its contents and the healthy profile',async()=>{
    await saved();
    const baseline=await native('load_session'),boot=await native('get_boot_status');
    const occupied=path.join(root,'occupied'),entries=fs.readdirSync(occupied),preserved=digest(path.join(occupied,'preserve.txt'));
    await prepareProfile('create','occupied');
    const dialog=await $('//div[@role="dialog"][.//h2[normalize-space()="Choose recovery storage"]]');
    await browser.waitUntil(async()=>(await dialog.getText()).includes('PROFILE_NOT_EMPTY'),{timeoutMsg:'An occupied native folder must be rejected rather than initialized'});
    await click('Keep current profile');await click('Done');
    assert.deepEqual(fs.readdirSync(occupied),entries);assert.equal(digest(path.join(occupied,'preserve.txt')),preserved);
    assert.deepEqual(await native('load_session'),baseline);
    assert.deepEqual((await native('get_boot_status')).profile,boot.profile);
    for(const original of inventory) assert.equal(digest(original.path),original.hash);
  });

  it('creates and reopens an independent native profile across renderer and process restart',async()=>{
    await saved();
    const baseline=await native('load_session'),baselineOperations=await native('list_operations'),defaultBoot=await native('get_boot_status');
    assert.equal(defaultBoot.profile.isDefault,true);
    const target=path.join(root,'recovery-profile');
    const created=await confirmProfile('create','recovery-profile',target);
    assert.notEqual(created.boot.profile.id,defaultBoot.profile.id);assert.equal(created.boot.profile.isDefault,false);
    assert.equal(created.loaded.session,null,'A newly created profile must not silently copy or save the old session');
    assert.deepEqual(await native('list_operations'),[],'Native operation authority must remain with the original profile');
    assert.ok(fs.existsSync(path.join(target,'.image-ranker-profile.json')));
    await assert.rejects(()=>native('save_session',{contextId:created.before.contextId,value:baseline}),/STALE_(?:PREVIEW_)?CONTEXT/);
    assert.equal(await native('load_session'),null,'An explicitly stale save must not alter the new profile');
    await $('[aria-label="Settings"]').click();
    const active=await $('[aria-label="Active storage profile"]');
    assert.ok((await active.getText()).includes(created.boot.profile.id));assertWindowsLocation(await active.$('p.break').getText(),target);
    const names=await $('//label[contains(.,"Show filenames below comparison images")]/input');await names.click();await saved();
    const independent=await native('load_session');assert.equal(independent.report.images.length,0);assert.equal(independent.run,null);
    await click('Done');await killAndReopen();
    assert.equal((await native('get_boot_status')).profile.id,created.boot.profile.id,'Independent launcher state must reopen the selected profile');
    assert.deepEqual(await native('load_session'),independent);
    const reopened=await confirmProfile('open','default',path.join(root,'app-data'));
    assert.equal(reopened.boot.profile.id,defaultBoot.profile.id);
    assert.deepEqual(await native('load_session'),baseline);assert.deepEqual(await native('list_operations'),baselineOperations);
    for(const original of inventory) assert.equal(digest(original.path),original.hash);
    fs.writeFileSync(path.join(root,'profile-switch-evidence.json'),JSON.stringify({evidenceClass:'native-profile-create-open-real-renderer-reload-and-process-reopen',defaultProfile:defaultBoot.profile,selectedProfile:created.boot.profile,created,reopened,independent},null,2));
  });

  it('restores the preserved recovery bundle into a healthy alternate profile without transferring file authority',async()=>{
    await saved();
    const defaultSession=await native('load_session'),defaultOperations=await native('list_operations'),defaultProfile=(await native('get_boot_status')).profile;
    const copyPath=path.join(root,'session-recovery.json'),copyHash=digest(copyPath);
    const bundle=JSON.parse(fs.readFileSync(copyPath,'utf8'));
    const expected=bundle.payload.snapshot.ui;
    assert.ok(expected?.run&&expected?.review,'The preserved bundle must contain the completed review and history');
    await prepareProfile('open','recovery-profile');
    await $('[aria-label="Prepared profile preview"]').waitForDisplayed();
    await click('Keep current profile');await click('Done');
    assert.deepEqual(await native('load_session'),defaultSession,'Cancelling a prepared profile must retain the current session');
    assert.equal((await native('get_boot_status')).profile.id,defaultProfile.id);
    // Reopening immediately also proves cancellation released the candidate's
    // real native writer lease instead of leaving it occupied by this window.
    const opened=await confirmProfile('open','recovery-profile',path.join(root,'recovery-profile'));
    const priorSession=opened.loaded.session;
    await $('[aria-label="Settings"]').click();await click('Open recovery copy');await click('Open recovery copy');
    const inspection=await $('.recovery-inspection');await inspection.waitForDisplayed();
    assert.ok((await inspection.getText()).includes('Trusted file journals unavailable'),'Imported operation references must remain explicitly non-authoritative');
    const beforeRestore=await markDocument();await $('button=Restore as a new session').waitForEnabled();await click('Restore as a new session');
    const restoredState=await afterCommittedReload(beforeRestore),restored=restoredState.loaded.session;
    assert.equal(restoredState.boot.profile.id,opened.boot.profile.id);
    assert.notEqual(restoredState.loaded.sessionGeneration,opened.loaded.sessionGeneration,'Recovery restore must create a separate branch');
    assert.deepEqual(restored.run,expected.run);assert.deepEqual(restored.review,expected.review);assert.deepEqual(restored.archivedRuns,expected.archivedRuns);
    assert.ok(restored.report.sources.every(source=>source.available===false));
    assert.deepEqual(await native('list_operations'),[],'Importing a bundle must not import runnable native journals');
    for(const operation of defaultOperations) await assert.rejects(()=>native('recover_operation',{id:operation.id,action:'resume'}),/Unknown operation/);
    for(const source of restored.report.sources) {
      fs.writeFileSync(path.join(root,'source-choice.txt'),path.basename(source.path)==='nested'?'nested':'source');
      await native('reconnect_source',{id:source.id});
    }
    await browser.refresh();await saved();
    const reconnected=await native('load_session');assert.ok(reconnected.report.sources.every(source=>source.available));
    assert.ok(reconnected.report.images.every(image=>image.availability==='ready'));assert.deepEqual(reconnected.run,expected.run);
    await killAndReopen();assert.equal((await native('get_boot_status')).profile.id,opened.boot.profile.id);
    assert.deepEqual((await native('load_session')).run,expected.run);
    await confirmProfile('open','default',path.join(root,'app-data'));
    assert.equal((await native('get_boot_status')).profile.id,defaultProfile.id);assert.deepEqual(await native('load_session'),defaultSession);
    assert.deepEqual(await native('list_operations'),defaultOperations);assert.equal(digest(copyPath),copyHash);
    for(const original of inventory) assert.equal(digest(original.path),original.hash);
    fs.writeFileSync(path.join(root,'profile-recovery-roundtrip-evidence.json'),JSON.stringify({evidenceClass:'preserved-bundle-native-healthy-profile-branch-restore-and-reconnect',copyPath,copyHash,sourceProfile:defaultProfile,targetProfile:opened.boot.profile,priorSession,sessionGenerationBefore:opened.loaded.sessionGeneration,sessionGenerationAfter:restoredState.loaded.sessionGeneration,missingJournalIds:defaultOperations.map(operation=>operation.id),restoredRun:restored.run.id},null,2));
  });

  it('exports a failed pending save before switching profiles and restores that action once in healthy storage',async()=>{
    await saved();
    const baseline=await native('load_session'),baselineStatus=await native('session_save_status'),sourceProfile=(await native('get_boot_status')).profile;
    const marker=path.join(root,'fail-session-append'),copyPath=path.join(root,'profile-recovery.json');
    const preservedCopy=path.join(root,'session-recovery.json'),preservedHash=digest(preservedCopy);
    assert.equal(fs.existsSync(marker),false);assert.equal(fs.existsSync(copyPath),false);
    fs.writeFileSync(path.join(root,'backup-choice.txt'),'profile');
    try {
      await $('[aria-label="Settings"]').click();
      const field=await $('//label[contains(.,"Show filenames below comparison images")]/input');
      fs.writeFileSync(marker,'image-ranker-e2e-injected-append-failure\n',{flag:'wx'});
      await field.click();
      await browser.waitUntil(async()=>(await $('.save-status').getText()).includes('Save failed'));
      const pending=await native('load_session'),pendingStatus=await native('session_save_status');
      assert.equal(pending.settings.showNames,!baseline.settings.showNames);
      assert.equal(pendingStatus.state,'pending');assert.equal(pendingStatus.durableRevision,baselineStatus.durableRevision);
      assert.ok(pendingStatus.pendingEventId);
      fs.writeFileSync(path.join(root,'profile-choice.txt'),'another-profile');
      const beforeSwitch=await markDocument();await click('Choose recovery storage');
      const preview=await $('[aria-label="Prepared profile preview"]');await preview.waitForDisplayed();
      assertWindowsLocation(await preview.$('p > strong').getText(),path.join(root,'another-profile'));
      assert.ok((await preview.getText()).includes('Pending work needs a decision'));
      assert.equal(await $('button=Switch profile').isExisting(),false,'Failed pending work must not receive an unqualified saved disposition');
      await preview.$('input[type="checkbox"]').click();await click('Save recovery copy and switch');
      const switched=await afterCommittedReload(beforeSwitch);
      assert.notEqual(switched.boot.profile.id,sourceProfile.id);assert.equal(switched.loaded.session,null);
      assertWindowsLocation(switched.boot.profile.displayPath,path.join(root,'another-profile'));
      assert.ok(fs.existsSync(copyPath));
      const exported=JSON.parse(fs.readFileSync(copyPath,'utf8')),copyHash=digest(copyPath);
      assert.deepEqual(exported.payload.snapshot.ui,pending,'The exported desired snapshot must retain all pending UI and history');
      assert.deepEqual(exported.payload.durableSnapshot.ui,baseline,'The export must distinguish the last acknowledged state');
      assert.equal(exported.payload.pendingEvent.event.id,pendingStatus.pendingEventId);
      assert.equal(exported.payload.pendingEvent.expectedRevision,baselineStatus.durableRevision);
      assert.equal(digest(preservedCopy),preservedHash,'An earlier exclusive recovery bundle must not be overwritten');
      fs.unlinkSync(marker); // Only the injected fault is removed; both bundles remain intact.
      await $('[aria-label="Settings"]').click();await click('Open recovery copy');await click('Open recovery copy');
      const pendingPanel=await $('.recovery-inspection .pending-actions');await pendingPanel.waitForDisplayed();
      const restore=await $('button=Restore as a new session');assert.equal(await restore.isEnabled(),false);
      await pendingPanel.$('input[type="checkbox"]').click();await restore.waitForEnabled();
      const beforeRestore=await markDocument();await restore.click();
      const restored=await afterCommittedReload(beforeRestore);
      assert.deepEqual(restored.loaded.session.run,pending.run);assert.deepEqual(restored.loaded.session.review,pending.review);
      assert.deepEqual(restored.loaded.session.archivedRuns,pending.archivedRuns);assert.deepEqual(restored.loaded.session.settings,pending.settings);
      assert.ok(restored.loaded.session.report.sources.every(source=>source.available===false));
      assert.equal(restored.loaded.saveStatus.pendingEventId,undefined);assert.equal(digest(copyPath),copyHash);
      const acknowledgedRevision=restored.loaded.saveStatus.durableRevision;
      await browser.refresh();await saved();
      assert.equal((await native('session_save_status')).durableRevision,acknowledgedRevision,'Renderer hydration must not apply the captured pending action again');
      assert.deepEqual((await native('load_session')).settings,pending.settings);
      await confirmProfile('open','default',path.join(root,'app-data'));
      assert.equal((await native('get_boot_status')).profile.id,sourceProfile.id);
      assert.deepEqual(await native('load_session'),baseline,'Export-and-switch must preserve the original profile at its acknowledged state');
      assert.deepEqual(await native('session_save_status'),baselineStatus);
      for(const original of inventory) assert.equal(digest(original.path),original.hash);
      fs.writeFileSync(path.join(root,'profile-pending-recovery-evidence.json'),JSON.stringify({evidenceClass:'injected-append-failure-frozen-bundle-profile-disposition-and-once-only-restore',sourceProfile,targetProfile:switched.boot.profile,pendingStatus,baselineStatus,copyPath,copyHash,preservedCopy,preservedHash,beforeSwitch,switchedContext:switched.loaded.contextId,restoredContext:restored.loaded.contextId,acknowledgedRevision,pendingSetting:pending.settings.showNames},null,2));
    } finally {
      if(fs.existsSync(marker))fs.unlinkSync(marker);
      fs.writeFileSync(path.join(root,'backup-choice.txt'),'default');
    }
  });

  it('persists operation defaults and exports the exact reviewed native diagnostic bytes',async()=>{
    await saved();
    await $('[aria-label="Settings"]').click();
    const manifest=await $('//label[contains(.,"Save JSON and CSV manifests by default")]/input');
    const readback=await $('//label[contains(.,"Verify copied bytes by reading outputs back by default")]/input');
    if(await manifest.isSelected()){await manifest.click();await saved();}
    if(!await readback.isSelected()){await readback.click();await saved();}
    const expectedSettings=(await native('load_session')).settings;
    assert.equal(expectedSettings.manifestDefault,false);assert.equal(expectedSettings.verifyCopyDefault,true);
    await click('Done');await killAndReopen();
    assert.deepEqual((await native('load_session')).settings,expectedSettings,'Defaults must survive a fresh native process');
    await click('Create ranked copies');
    assert.equal(await $('//label[contains(.,"Save ranking manifests (JSON and CSV)")]/input').isSelected(),false);
    assert.equal(await $('//label[contains(.,"Verify copied bytes by reading each output back")]/input').isSelected(),true);
    await click('Cancel');

    await $('[aria-label="Help and keyboard shortcuts"]').click();
    await $('//h3[starts-with(normalize-space(),"About Image Ranker")]').waitForDisplayed();
    const info=await native('app_info');
    assert.ok((await $('[role="dialog"]').getText()).includes(`Version ${info.version}`));
    assert.ok((await $('[role="dialog"]').getText()).includes('Image decoding policy'));
    await checkProtectionSurface();
    const notice=await $('//details[summary[contains(.,"objc2 0.6.4")]]');
    await notice.$('summary').scrollIntoView();await notice.$('summary').click();
    assert.ok((await notice.$('.license-text').getText()).includes('Apple SDKs'),'Bundled upstream licensing caveats must remain visible');
    const provenance=JSON.parse(fs.readFileSync('assets/third-party-notices.provenance.json','utf8'));
    assert.ok(provenance.reviewRequired.some(entry=>entry.id==='cargo:selectors@0.38.0'),'Notice capture must not erase release-review flags');
    await click('Preview diagnostics');
    const preview=await $('[aria-label="Exact diagnostic export preview"]');await preview.waitForDisplayed();
    const exactText=await preview.getProperty('textContent');
    assert.equal(JSON.parse(exactText).kind,'image-ranker-redacted-diagnostics');
    assert.equal(exactText.includes(root),false,'Default diagnostic preview must redact fixture paths');
    const output=path.join(root,'diagnostics.json');assert.equal(fs.existsSync(output),false,'Diagnostic export fixture must start unoccupied');
    await click('Save displayed diagnostics');
    await browser.waitUntil(async()=>fs.existsSync(output)&&(await $('[role="dialog"]').getText()).includes('Displayed diagnostics saved:'),{timeoutMsg:'Expected the reviewed diagnostic capture to be published'});
    assert.deepEqual(fs.readFileSync(output),Buffer.from(exactText,'utf8'),'Export must save the displayed frozen bytes, including whitespace');
    assert.equal(fs.statSync(output).size,Buffer.byteLength(exactText,'utf8'));
    await click('Done');
  });

  it('retains displayed preview pins through clear and retires stale contexts after a real renderer reload',async()=>{
    await saved();
    const baseline=await native('load_session');
    assert.equal(baseline.screen,'review');assert.equal(baseline.review.order.length,1,'This lifecycle fixture has exactly one displayed thumbnail');
    const displayed=baseline.report.images.find(image=>image.id===baseline.review.order[0]);
    const offscreen=baseline.report.images.find(image=>image.id!==displayed.id&&image.availability==='ready');
    assert.ok(offscreen);
    const previewState=()=>browser.execute(()=>{
      const image=document.querySelector('.review-card .thumbnail img');
      return image?{src:image.currentSrc,complete:image.complete,width:image.naturalWidth,height:image.naturalHeight}:null;
    });
    const readyPreview=async()=>{
      await browser.waitUntil(async()=>{const image=await previewState();return !!image?.complete&&image.width>0&&image.height>0;},{timeoutMsg:'Expected a decoded native review preview'});
      return previewState();
    };
    // A fresh query forces another asset-protocol read, rather than accepting
    // pixels already decoded in the visible <img> before cache clearing.
    const readFreshPixels=async(src)=>{
      const {result}=await browser.executeAsync((url,nonce,done)=>{
        const image=new Image();
        const timer=setTimeout(()=>done({result:{ok:false,error:'Fresh preview asset read timed out'}}),10000);
        image.onload=()=>image.decode().then(()=>{clearTimeout(timer);done({result:{ok:true,width:image.naturalWidth,height:image.naturalHeight}});},error=>{clearTimeout(timer);done({result:{ok:false,error:String(error)}});});
        image.onerror=()=>{clearTimeout(timer);done({result:{ok:false,error:'Fresh preview asset read failed'}});};
        image.src=`${url}${url.includes('?')?'&':'?'}lifecycle=${nonce}`;
      },src,randomUUID());
      assert.equal(result.ok,true,result.error);assert.equal(result.width,displayed.width);assert.equal(result.height,displayed.height);
    };
    const args=(image,contextId,size=512)=>({id:image.id,size,expectedContentRevision:image.contentRevision,requestId:randomUUID(),contextId});
    const leases=new Set();
    const acquire=async(input)=>{const preview=await native('request_preview',input);leases.add(preview.leaseId);return preview;};
    const release=async(leaseId)=>{await native('release_preview',{leaseId});leases.delete(leaseId);};
    // FIX 1: An inventory eviction unit is its PNG plus metadata sidecar;
    // lease accounting measures the pinned PNG only. Preserve both hashes.
    const unitSnapshot=pngPath=>{
      assert.equal(path.extname(pngPath),'.png');
      const metadataPath=path.join(path.dirname(pngPath),`${path.basename(pngPath,'.png')}.meta.json`);
      const record=file=>{
        const stat=fs.statSync(file);assert.ok(stat.isFile());
        return {path:file,sizeBytes:stat.size,sha256:digest(file)};
      };
      const png=record(pngPath),metadata=record(metadataPath);
      return {png,metadata,sizeBytes:png.sizeBytes+metadata.sizeBytes};
    };
    const checkpoints=[];
    const checkpoint=(stage,details)=>{
      assert.ok(checkpoints.length<8,'Lifecycle evidence must remain bounded');
      checkpoints.push({stage,...details});
      fs.writeFileSync(path.join(root,'preview-lifecycle-progress.json'),JSON.stringify({evidenceClass:'native-cache-accounting-checkpoints',accounting:'inventory and retainedBytes include PNG plus metadata; pinnedBytes include PNG only',checkpoints},null,2));
    };
    try {
      const beforeProcess=await supervise('inspect');
      const oldContext=await native('preview_context');
      const oldPixels=await readyPreview();
      const visibleProbe=await acquire(args(displayed,oldContext));
      const visibleAsset=await browser.execute(file=>window.__TAURI__.core.convertFileSrc(file),visibleProbe.path);
      assert.equal(oldPixels.src.split('?')[0],visibleAsset,'The native lease must refer to the actual displayed file');
      await release(visibleProbe.leaseId); // Only the application's display lease now holds this file.
      const retiredProbe=await acquire(args(offscreen,oldContext,384));
      const delayedRequest=args(offscreen,oldContext,384); // Capture before reload; dispatch only after the new renderer is ready.
      const heldPaths=[visibleProbe.path,retiredProbe.path];
      assert.equal(new Set(heldPaths).size,2);
      const heldUnits=heldPaths.map(unitSnapshot);
      const heldArtifacts=heldUnits.flatMap(unit=>[unit.png.path,unit.metadata.path]);
      const heldPngBytes=heldUnits.reduce((sum,unit)=>sum+unit.png.sizeBytes,0);
      const heldUnitBytes=heldUnits.reduce((sum,unit)=>sum+unit.sizeBytes,0);
      const heldHashes=heldPaths.map(file=>digest(file));
      checkpoint('before-clear',{heldUnits,heldPngBytes,heldUnitBytes});
      const precedingClear=(await native('cache_info',{refresh:false})).clearRequest;
      if(precedingClear) assert.ok(['completed','completed-with-retained-items','cancelled','failed'].includes(precedingClear.state),'The fixture must not inherit an unresolved clear with a different admitted scope');
      await $('[aria-label="Settings"]').click();await click('Clear preview cache');
      let admittedClear;
      await browser.waitUntil(async()=>{
        admittedClear=(await native('cache_info',{refresh:false})).clearRequest;
        return !!admittedClear&&admittedClear.requestId!==precedingClear?.requestId;
      },{timeoutMsg:'Clear must publish a new native receipt before Settings can track it'});
      assertClearReceipt(admittedClear);assert.equal(admittedClear.retryOf,null);
      const cleared=await waitForClear(admittedClear,'waiting-for-pins',value=>value.inventoryKnown&&!value.maintenanceRunning&&value.files===2&&value.pinnedFiles===2&&value.clearRequest.retainedCountsKnown&&value.clearRequest.retainedFiles===2);
      checkpoint('first-clear-waiting',{admittedClear,cleared,heldUnits:heldPaths.map(unitSnapshot)});
      assert.equal(cleared.files,2);assert.equal(cleared.pinnedFiles,2);
      // FIX 2: Exact totals retain the separate native inventory/lease units.
      assert.equal(cleared.sizeBytes,heldUnitBytes);assert.equal(cleared.pinnedBytes,heldPngBytes);
      assert.equal(cleared.clearRequest.retainedBytes,heldUnitBytes);
      const clearStatus=await assertWaitingCacheSurface(2);
      assert.deepEqual(heldPaths.map(file=>digest(file)),heldHashes,'Clear must retain the exact pinned PNG bytes');
      assert.deepEqual(heldPaths.map(unitSnapshot),heldUnits,'Clear must also preserve the exact metadata of pinned eviction units');
      await click('Done');
      assert.deepEqual(await readyPreview(),oldPixels,'Cache clear must preserve the visible preview without swapping its asset');
      await readFreshPixels(oldPixels.src);

      await browser.refresh(); // Real page navigation: native on_page_load must rotate context and retire old leases.
      await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();await saved();
      const currentPixels=await readyPreview();
      const currentContext=await native('preview_context');
      assert.notEqual(currentContext,oldContext);
      const afterProcess=await supervise('inspect');assert.equal(afterProcess.processId,beforeProcess.processId,'This is a renderer reload, not a process restart');
      await browser.waitUntil(()=>heldArtifacts.every(file=>!fs.existsSync(file)),{timeoutMsg:'Reload must release old-context leases and collect each PNG and sidecar in the cleared generation'});
      const completedClear=(await waitForClear(admittedClear,'completed',value=>value.clearRequest.retainedCountsKnown)).clearRequest;
      checkpoint('first-clear-completed-after-reload',{completedClear,retiredArtifacts:heldArtifacts.map(file=>({path:file,exists:fs.existsSync(file)}))});
      assert.equal(completedClear.retainedFiles,0);assert.equal(completedClear.retainedBytes,0);assert.deepEqual(completedClear.reasons,[]);
      assert.notEqual(currentPixels.src.split('?')[0],oldPixels.src.split('?')[0]);
      await readFreshPixels(currentPixels.src);
      const currentProbe=await acquire(args(displayed,currentContext));
      const currentAsset=await browser.execute(file=>window.__TAURI__.core.convertFileSrc(file),currentProbe.path);
      assert.equal(currentPixels.src.split('?')[0],currentAsset);
      await release(currentProbe.leaseId);
      const currentUnit=unitSnapshot(currentProbe.path);
      // FIX 3: A completed old generation and a pinned current generation can
      // coexist. Remount Settings and require both facts to stay visible.
      await $('[aria-label="Settings"]').click();
      await browser.waitUntil(async()=>(await $('.cache-settings').getText()).includes('1 files remain, including 1 displayed or prefetched previews.'),{timeoutMsg:'The completed receipt must not claim that current-generation pinned bytes were deleted'});
      assert.ok((await $('.cache-settings .success').getText()).includes('Unused cache entries cleared.'));
      await click('Done');
      const currentPins=await waitForClear(admittedClear,'completed',value=>value.inventoryKnown&&!value.maintenancePending&&!value.maintenanceRunning);
      checkpoint('current-generation-pin',{currentPins,currentUnit});
      assert.equal(currentPins.files,1);assert.equal(currentPins.sizeBytes,currentUnit.sizeBytes);
      assert.equal(currentPins.pinnedFiles,1);assert.equal(currentPins.pinnedBytes,currentUnit.png.sizeBytes);
      await assert.rejects(()=>native('request_preview',delayedRequest),/\bSTALE_CONTEXT\b/);
      assert.deepEqual(await native('cache_info'),currentPins,'A delayed old-context request must not release or replace active pins');
      await release(retiredProbe.leaseId);await release(retiredProbe.leaseId);
      assert.deepEqual(await native('cache_info'),currentPins,'Late, duplicate cleanup from a retired context must not affect current leases');
      assert.deepEqual(await readyPreview(),currentPixels);

      const freshProbe=await acquire(args(offscreen,currentContext,384));
      assert.notEqual(freshProbe.path,retiredProbe.path,'A new request must use the current cache generation');
      const freshUnit=unitSnapshot(freshProbe.path);
      const secondAdmission=await native('clear_cache');assertClearReceipt(secondAdmission.clearRequest);
      assert.notEqual(secondAdmission.clearRequest.requestId,admittedClear.requestId);
      assert.notEqual(secondAdmission.clearRequest.generation,admittedClear.generation);
      const clearedAgain=await waitForClear(secondAdmission.clearRequest,'waiting-for-pins',value=>value.inventoryKnown&&!value.maintenanceRunning&&value.files===2&&value.pinnedFiles===2&&value.clearRequest.retainedCountsKnown&&value.clearRequest.retainedFiles===2);
      checkpoint('second-clear-waiting',{secondAdmission,clearedAgain,currentUnit:unitSnapshot(currentProbe.path),freshUnit:unitSnapshot(freshProbe.path)});
      assert.equal(clearedAgain.files,2);assert.equal(clearedAgain.pinnedFiles,2);
      assert.equal(clearedAgain.sizeBytes,currentUnit.sizeBytes+freshUnit.sizeBytes);
      assert.equal(clearedAgain.pinnedBytes,currentUnit.png.sizeBytes+freshUnit.png.sizeBytes);
      assert.equal(clearedAgain.clearRequest.retainedBytes,currentUnit.sizeBytes+freshUnit.sizeBytes);
      assert.deepEqual(unitSnapshot(currentProbe.path),currentUnit);assert.deepEqual(unitSnapshot(freshProbe.path),freshUnit);
      await release(freshProbe.leaseId);await release(freshProbe.leaseId);
      await browser.waitUntil(()=>!fs.existsSync(freshProbe.path)&&!fs.existsSync(freshUnit.metadata.path),{timeoutMsg:'The last release must collect the cleared offscreen PNG and metadata'});
      assert.equal(fs.existsSync(freshProbe.path),false,'The last release must collect a cleared preview');
      await waitForClear(secondAdmission.clearRequest,'waiting-for-pins',value=>value.inventoryKnown&&!value.maintenanceRunning&&value.clearRequest.retainedCountsKnown&&value.clearRequest.retainedFiles===1&&value.files===1);
      await $('[aria-label="Settings"]').click();
      const finalClearStatus=await assertWaitingCacheSurface(1);
      await click('Done');
      const finalCache=await waitForClear(secondAdmission.clearRequest,'waiting-for-pins',value=>value.inventoryKnown&&!value.maintenanceRunning&&value.clearRequest.retainedCountsKnown&&value.clearRequest.retainedFiles===1);
      // FIX 3: Save diagnostics before final byte assertions, including sidecar
      // hashes, so a failed run retains the exact observed accounting inputs.
      checkpoint('only-current-display-retained',{finalCache,currentUnit:unitSnapshot(currentProbe.path),releasedUnit:freshUnit});
      assert.equal(finalCache.files,1);assert.equal(finalCache.pinnedFiles,1);
      assert.equal(finalCache.sizeBytes,currentUnit.sizeBytes);assert.equal(finalCache.pinnedBytes,currentUnit.png.sizeBytes);
      assert.equal(finalCache.clearRequest.retainedBytes,currentUnit.sizeBytes);
      assert.deepEqual(unitSnapshot(currentProbe.path),currentUnit,'The still-displayed PNG and metadata must retain their original hashes');
      assert.deepEqual((await native('cache_info',{refresh:false,requestId:admittedClear.requestId})).clearRequest,completedClear,'A newer clear must not rewrite the first terminal receipt');
      assert.equal((await native('cache_info',{refresh:false,requestId:randomUUID()})).clearRequest,null,'An unknown exact lookup must not substitute the latest clear');
      await assert.rejects(()=>native('request_preview',{...delayedRequest,requestId:randomUUID()}),/\bSTALE_CONTEXT\b/);
      await release(retiredProbe.leaseId);
      assert.deepEqual(await native('cache_info'),finalCache);
      for(const file of [...heldArtifacts,freshProbe.path,freshUnit.metadata.path]) assert.equal(fs.existsSync(file),false,'Rejected stale work and cleanup must not resurrect a cleared PNG or metadata sidecar');
      await readFreshPixels(currentPixels.src);
      const restored=await native('load_session');assert.deepEqual(restored.run,baseline.run);assert.deepEqual(restored.review,baseline.review);
      for(const original of inventory) assert.equal(digest(original.path),original.hash);
      fs.writeFileSync(path.join(root,'preview-lifecycle-evidence.json'),JSON.stringify({evidenceClass:'native-exact-clear-receipts-real-renderer-reload-and-delayed-old-context-dispatch',receiptScope:'regular final-cache PNG-plus-metadata eviction units; PNG-only pinned bytes; not decoder temporary-output cleanup or worker reap',beforeProcess,afterProcess,oldContext,currentContext,heldPaths,heldUnits,heldPngBytes,heldUnitBytes,currentUnit,freshUnit,clearStatus,admittedClear,cleared,completedClear,currentPins,secondAdmission,clearedAgain,finalClearStatus,finalCache,oldPixels,currentPixels,retiredPaths:[...heldArtifacts,freshProbe.path,freshUnit.metadata.path],checkpoints},null,2));
    } finally {
      for(const leaseId of leases) await native('release_preview',{leaseId});
    }
  });

  it('keeps real helper-held image work observable and cancellable through renderer retirement',async()=>{
    await saved();
    const baseline=await native('load_session');
    const image=baseline.report.images.find(item=>item.availability==='ready'&&!baseline.review.order.includes(item.id));
    assert.ok(image,'The held decode must use an offscreen source without disturbing the visible comparison');
    const process=await supervise('inspect');
    const holdFile=path.join(root,'image-worker-hold.json');
    const heldRequests=[];
    const evidence={evidenceClass:'actual-native-helper-held-ipc-cancel-and-real-renderer-reload',process,sourceId:image.id,stages:[]};
    const inspectWorker=inspectDecoderProcess;
    const idle=async()=>{
      let last;
      await browser.waitUntil(async()=>{
        last=await native('image_work_status');
        return last.active===0&&last.decoder.active===0&&last.decoder.pending===0&&last.decoder.reservedBytes===0&&last.decoder.liveOutputs===0&&last.decoder.outputReservedBytes===0;
      },{timeout:15000,timeoutMsg:'Canceled image work must return only after native worker/output reservations are released'});
      assert.equal(last.decoder.unavailable,false,'An unconfirmed worker exit cannot be treated as successful cleanup');
      return last;
    };
    const beginHeld=async(stage)=>{
      await idle();
      const contextId=await native('preview_context'),requestId=randomUUID(),nonce=randomUUID();
      fs.writeFileSync(holdFile,JSON.stringify({requestId,nonce,stage}),{flag:'wx'});
      const request={id:image.id,size:384,expectedContentRevision:image.contentRevision,requestId,contextId};
      heldRequests.push(request);
      await browser.execute((input,key)=>{
        window.__imageRankerHeldRequests??={};
        window.__imageRankerHeldRequests[key]={state:'pending'};
        void window.__TAURI__.core.invoke('request_preview',input).then(
          value=>{window.__imageRankerHeldRequests[key]={state:'resolved',value};},
          error=>{window.__imageRankerHeldRequests[key]={state:'rejected',error:String(error)};},
        );
      },request,requestId);
      let status;
      await browser.waitUntil(async()=>{
        status=await native('image_work_status');
        return status.decoder.stage===`fixture-held:${nonce}:${stage}`;
      },{timeout:15000,timeoutMsg:'Expected a nonce-bound acknowledgment from the actual decoder child, not a parent preparation barrier'});
      assert.equal(fs.existsSync(holdFile),false,'Only the admitted matching request consumes the one-shot fixture');
      assert.equal(status.decoder.active,1);
      assert.ok(status.decoder.reservedBytes>0);
      assert.ok(status.jobs.some(job=>job.requestId===requestId&&job.contextId===contextId));
      const worker=await inspectWorker(status.decoder.workerPid);
      assert.ok(worker,'The held acknowledgment must name a live OS process');
      assert.equal(worker.ParentProcessId,process.processId);
      assertWindowsLocation(path.resolve(worker.ExecutablePath).toLowerCase(),path.resolve('src-tauri/target/debug/image-ranker.exe').toLowerCase());
      assert.ok(worker.CommandLine.includes('--image-ranker-decode-worker='));
      assert.notEqual(worker.ProcessId,process.processId);
      return {request,nonce,stage,status,worker};
    };
    const measure=async(command,args,count)=>{
      const {result}=await browser.executeAsync((cmd,input,repeats,done)=>{
        void(async()=>{
          const samples=[];
          for(let index=0;index<repeats;index++){
            const started=performance.now();
            const value=await window.__TAURI__.core.invoke(cmd,input);
            samples.push({milliseconds:performance.now()-started,value});
          }
          return samples;
        })().then(samples=>done({result:{ok:true,samples}}),error=>done({result:{ok:false,error:String(error)}}));
      },command,args,count);
      assert.equal(result.ok,true,result.error);
      const sorted=result.samples.map(sample=>sample.milliseconds).sort((a,b)=>a-b);
      const p95=sorted[Math.ceil(sorted.length*.95)-1];
      assert.ok(p95<=250,`${command} IPC acknowledgment p95 ${p95.toFixed(2)}ms exceeds 250ms`);
      return {samples:result.samples,p95Milliseconds:p95};
    };
    const probeReady=async()=>{
      const preview=await native('request_preview',{id:image.id,size:384,expectedContentRevision:image.contentRevision,requestId:randomUUID()});
      try{
        assert.equal(preview.status,'ready');assert.equal(preview.contentRevision,image.contentRevision);
        assert.ok(preview.leaseId&&fs.existsSync(preview.path));
      }finally{await native('release_preview',{leaseId:preview.leaseId});}
      return {path:preview.path,contentRevision:preview.contentRevision};
    };
    try{
      const held=await beginHeld('decode');
      const statusTiming=await measure('image_work_status',{contextId:held.request.contextId},20);
      for(const sample of statusTiming.samples){
        assert.equal(sample.value.decoder.stage,`fixture-held:${held.nonce}:decode`);
        assert.equal(sample.value.decoder.workerPid,held.worker.ProcessId);
        assert.equal(sample.value.decoder.active,1);
        assert.ok(sample.value.decoder.reservedBytes>0,'A live held worker must retain its reservation');
      }
      const cancellationStarted=Date.now();
      const cancelTiming=await measure('cancel_image_work',{contextId:held.request.contextId,requestId:held.request.requestId},20);
      assert.equal(cancelTiming.samples[0].value,1,'First cancellation must target the actual held request');
      assert.ok(cancelTiming.samples[0].milliseconds<=250,'The first real cancellation acknowledgment must also meet the bound');
      const afterCancel=await idle();
      const cancellationToIdleMs=Date.now()-cancellationStarted;
      assert.equal(await inspectWorker(held.worker.ProcessId),null,'Zero reservations require OS evidence that the held child has exited');
      await browser.waitUntil(()=>browser.execute(key=>window.__imageRankerHeldRequests[key]?.state!=='pending',held.request.requestId));
      const {result}=await browser.execute(key=>({result:window.__imageRankerHeldRequests[key]}),held.request.requestId);
      assert.equal(result.state,'rejected');assert.match(result.error,/IMAGE_JOB_CANCELLED|DECODE_CANCELLED/);
      const successfulAfterCancel=await probeReady();
      evidence.stages.push({...held,statusTiming,cancelTiming,cancellationToIdleMs,afterCancel,result,successfulAfterCancel});

      const retired=await beginHeld('resize');
      const beforeReload=await markDocument();
      await browser.refresh();
      const afterReload=await afterCommittedReload(beforeReload);
      const afterRetirement=await idle();
      assert.equal(await inspectWorker(retired.worker.ProcessId),null,'A real renderer reload must stop and reap its obsolete helper');
      assert.equal(await browser.execute(()=>typeof window.__imageRankerHeldRequests==='undefined'),true,'The old request observer belongs to the discarded document');
      await browser.waitUntil(()=>browser.execute(()=>{const preview=document.querySelector('.review-card .thumbnail img');return !!preview?.complete&&preview.naturalWidth>0;}),{timeoutMsg:'The current renderer must display usable preview pixels after retiring held work'});
      await idle();
      const cacheBeforeStale=await native('cache_info');
      await assert.rejects(()=>native('request_preview',{...retired.request,requestId:randomUUID()}),/\bSTALE_CONTEXT\b/);
      assert.deepEqual(await native('cache_info'),cacheBeforeStale,'Old-context dispatch cannot publish or replace a current lease');
      const successfulAfterReload=await probeReady();
      evidence.stages.push({...retired,beforeReload,afterReload,afterRetirement,cacheBeforeStale,successfulAfterReload});
      const restored=await native('load_session');
      assert.deepEqual(restored.run,baseline.run);assert.deepEqual(restored.review,baseline.review);
      for(const original of inventory)assert.equal(digest(original.path),original.hash);
      fs.writeFileSync(path.join(root,'image-worker-responsiveness-evidence.json'),JSON.stringify(evidence,null,2));
    }finally{
      if(fs.existsSync(holdFile))fs.unlinkSync(holdFile);
      for(const request of heldRequests){
        // Current-context cancellation names an exact request ID; retired IDs are
        // harmless misses. The native context lifecycle owns old-worker teardown.
        await native('cancel_image_work',{requestId:request.requestId});
        const {result}=await browser.execute(key=>({result:window.__imageRankerHeldRequests?.[key]}),request.requestId);
        if(result?.state==='resolved'&&result.value?.leaseId)await native('release_preview',{leaseId:result.value.leaseId});
      }
    }
  });

  it('keeps a failed save through native WM_CLOSE, then saves and exits after retry',async()=>{
    // This is injected I/O failure plus a real normal window close. It is not
    // process-kill, storage-power-loss or filesystem atomicity evidence.
    await saved();
    const baseline=await native('load_session');
    const managedEvidence=()=>{
      const base=path.join(root,'app-data');
      const walk=directory=>fs.readdirSync(directory,{withFileTypes:true}).flatMap(entry=>{
        const file=path.join(directory,entry.name);
        assert.equal(entry.isSymbolicLink(),false,'Managed fixture evidence must not follow links');
        if(entry.isDirectory())return walk(file);
        assert.equal(entry.isFile(),true,'Unexpected managed fixture artifact type');
        const relative=path.relative(base,file).split(path.sep).join('/');
        // These exact artifacts are created empty by acquire_writer/SessionStore
        // and held under OS locks. Stat them without reading locked file data;
        // every other state, journal, authority and artifact must remain hashable.
        const knownLock=relative==='writer.lock'||relative==='sessions/session.lock'||/^branches\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\/sessions\/session\.lock$/i.test(relative);
        if(knownLock){
          const observed=fs.lstatSync(file,{bigint:true});
          assert.equal(observed.isFile(),true,'A native lock artifact must remain a regular file');
          assert.equal(observed.size,0n,'Known native lock artifacts must remain empty');
          return [{path:relative,evidence:'native-lock-stat',sizeBytes:0,identity:{device:observed.dev.toString(),file:observed.ino.toString()},birthtimeNs:observed.birthtimeNs.toString()}];
        }
        return [{path:relative,sha256:digest(file)}];
      });
      return walk(base).sort((a,b)=>a.path.localeCompare(b.path));
    };
    const durableBefore=managedEvidence();
    const baselineStatus=await native('session_save_status');
    assert.equal(baselineStatus.state,'saved');
    assert.ok(Number.isSafeInteger(baselineStatus.durableRevision),'This persisted fixture must expose an acknowledged revision');
    assert.deepEqual(managedEvidence(),durableBefore,'A healthy status probe must not write managed files');
    const before=await supervise('inspect');
    assert.equal(before.running,true);assert.equal(before.windowPid,before.processId);
    const marker=path.join(root,'fail-session-append');
    assert.equal(fs.existsSync(marker),false,'The session append failure marker must start absent');
    await $('[aria-label="Settings"]').click();
    let field=await $('//label[contains(.,"Show filenames below comparison images")]/input');
    const desired=!baseline.settings.showNames;
    try {
      fs.writeFileSync(marker,'image-ranker-e2e-injected-append-failure\n',{flag:'wx'});
      await field.click();
      await browser.waitUntil(async()=>(await $('.save-status').getText()).includes('Save failed'),{timeoutMsg:'Expected the fixture-induced append failure'});
      assert.ok((await $('.error-bar').getText()).includes('injected desktop session append failure'));
      assert.equal(await field.isSelected(),desired,'Failed saves must retain the pending setting in memory');
      assert.equal((await native('load_session')).settings.showNames,desired,'The live native workspace retains pending UI state');
      const durableAfterFailure=managedEvidence();
      assert.deepEqual(durableAfterFailure,durableBefore,'Failed append must leave actual managed session and journal bytes unchanged');
      const pendingStatus=await native('session_save_status');
      assert.equal(pendingStatus.state,'pending');
      assert.equal(pendingStatus.durableRevision,baselineStatus.durableRevision);
      assert.equal(typeof pendingStatus.pendingEventId,'string');assert.ok(pendingStatus.pendingEventId.length>0);
      const durableAfterProbe=managedEvidence();
      assert.deepEqual(durableAfterProbe,durableAfterFailure,'Pending status inspection must not append, replay, repair or acknowledge the event');
      const rendererBefore=await native('preview_context');
      let reloadCommandError=null;
      try { await browser.refresh(); } catch(error) {
        // Accept only an actual before-unload prompt; do not remove the app's
        // unload guard or treat a cancelled navigation as reload evidence.
        if(!/unexpected alert open/i.test(String(error))||!await browser.isAlertOpen())throw error;
        reloadCommandError=String(error);await browser.acceptAlert();
      }
      if(await browser.isAlertOpen())await browser.acceptAlert();
      await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();
      await browser.waitUntil(async()=>(await native('preview_context'))!==rendererBefore,{timeoutMsg:'Expected a genuine renderer reload with a newly issued native context'});
      const rendererAfter=await native('preview_context');
      await browser.waitUntil(async()=>(await $('.save-status').getText()).includes('Save failed'),{timeoutMsg:'Hydration must retain native pending status after renderer reload'});
      const reloaded=await supervise('inspect',before.processId);
      assert.equal(reloaded.running,true);assert.equal(reloaded.processId,before.processId,'Hydration proof must keep the same native process and its pending event');
      const pendingAfterReload=await native('session_save_status');
      assert.deepEqual(pendingAfterReload,pendingStatus,'Reload must preserve the pending event identity and acknowledged revision');
      const durableAfterReload=managedEvidence();
      assert.deepEqual(durableAfterReload,durableBefore,'Renderer hydration and status probes must leave durable managed evidence unchanged');
      const hydrated=await native('load_session');
      assert.equal(hydrated.settings.showNames,desired);assert.deepEqual(hydrated.run,baseline.run);assert.deepEqual(hydrated.review,baseline.review);
      await $('[aria-label="Settings"]').click();
      field=await $('//label[contains(.,"Show filenames below comparison images")]/input');
      assert.equal(await field.isSelected(),desired,'The fresh renderer must display the pending setting without silently discarding or saving it');
      const requested=await supervise('WM_CLOSE',before.processId);
      assert.equal(requested.closeRequestedPid,before.processId);assert.equal(requested.windowPid,before.processId);
      const closeDialog=await $('//div[@role="dialog"][.//h2[normalize-space()="Close Image Ranker"]]');
      await closeDialog.waitForDisplayed();
      const retry=await closeDialog.$('button=Retry save and close');await retry.waitForEnabled();
      assert.ok((await closeDialog.getText()).includes('Your latest work remains visible in memory'));
      assert.equal(await field.isSelected(),desired);
      const retained=await supervise('inspect',before.processId);assert.equal(retained.running,true);assert.equal(retained.processId,before.processId);
      fs.unlinkSync(marker);
      let closeCommandError=null;
      try { await retry.click(); } catch(error) {
        // WebDriver may lose the webview before its click response arrives.
        // Only a real supervised exit and successful reload below qualify it.
        if(!/no such window|invalid session|disconnected|target window already closed|web view not found/i.test(String(error)))throw error;
        closeCommandError=String(error);
      }
      let exited;
      await browser.waitUntil(async()=>{exited=await supervise('inspect',before.processId);return !exited.running;},{timeout:20000,interval:500,timeoutMsg:'The exact native process must exit only after its retried save is acknowledged'});
      assert.equal(exited.processId,before.processId);
      await browser.reloadSession();
      await $('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed();await saved();
      const reopened=await native('load_session');
      assert.equal(reopened.settings.showNames,desired,'Retry and normal close must durably persist the pending setting');
      assert.deepEqual(reopened.run,baseline.run,'Close recovery must not discard ranking history');
      assert.deepEqual(reopened.review,baseline.review,'Close recovery must not discard manual review edits');
      const after=await supervise('inspect');assert.equal(after.running,true);assert.notEqual(after.processId,before.processId);
      const reopenedStatus=await native('session_save_status');
      assert.equal(reopenedStatus.state,'saved');assert.equal(reopenedStatus.pendingEventId,undefined);
      assert.equal(reopenedStatus.durableRevision,baselineStatus.durableRevision+1,'Exactly one acknowledged session event must be added by retry; status, hydration and close must not duplicate it');
      fs.writeFileSync(path.join(root,'normal-close-retry-evidence.json'),JSON.stringify({evidenceClass:'injected-session-append-failure-real-renderer-reload-and-native-WM_CLOSE',before,reloaded,requested,retained,exited,after,closeCommandError,reloadCommandError,rendererBefore,rendererAfter,baselineStatus,pendingStatus,pendingAfterReload,reopenedStatus,acknowledgedEventDelta:reopenedStatus.durableRevision-baselineStatus.durableRevision,setting:'showNames',durableSettingBefore:baseline.settings.showNames,pendingThenSaved:desired,durableBefore,durableAfterFailure,durableAfterProbe,durableAfterReload},null,2));
    } finally {
      if(fs.existsSync(marker))fs.unlinkSync(marker);
    }
  });
});
