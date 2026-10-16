import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {digest} from './fixtures.mjs';
import {summarizeLatency} from '../qualification/latency.mjs';
import {checkpointByteLimit,checkpointInterval,checkpointMetadata,checkpointExpectation,verifyCheckpointChange,checkpointSubsets} from '../qualification/decision-checkpoints.mjs';
import {captureBuildKind} from '../qualification/capture-release-e2e.mjs';
import {createDecisionTransport} from '../qualification/decision-transport.mjs';

const root=fs.realpathSync(process.env.IMAGE_RANKER_E2E_ROOT);
assert.equal(fs.readFileSync(path.join(root,'.image-ranker-e2e'),'utf8'),'image-ranker-disposable-fixture-v1\n');
function owned(file){
  const actual=fs.realpathSync(file),relative=path.relative(root,actual);
  assert.ok(relative&&!relative.startsWith(`..${path.sep}`)&&relative!=='..'&&!path.isAbsolute(relative),'Evidence reads must stay in the owned disposable fixture');
  return actual;
}
const workloadFile=owned(path.join(root,'decision-workload.json'));
const inventoryFile=owned(path.join(root,'inventory.json'));
const domainFile=owned(path.join(root,'qualification-domain.mjs'));
const workload=JSON.parse(fs.readFileSync(workloadFile,'utf8'));
const inventory=JSON.parse(fs.readFileSync(inventoryFile,'utf8'));
const {createRun,decide,assertValidRun,undo}=await import(pathToFileURL(domainFile));
let stopped=false;
function live(){if(stopped)throw new Error('Decision sampler stopped; native action ownership has not been reconciled');}
async function native(command,args={}){
  live();
  const {result}=await browser.executeAsync((cmd,input,done)=>{
    void(async()=>{
      const contextId=await window.__TAURI__.core.invoke('preview_context');
      return window.__TAURI__.core.invoke(cmd,{contextId,...input});
    })().then(value=>done({result:{ok:true,value}}),error=>done({result:{ok:false,error:String(error)}}));
  },command,args);
  if(!result.ok)throw new Error(`${command}: ${result.error}`);
  return result.value;
}
const saved=()=>browser.waitUntil(async()=>(await native('session_save_status')).state==='saved');
function originals(){for(const image of inventory)assert.equal(digest(owned(image.path)),image.hash);}
function record(name,value){fs.writeFileSync(path.join(root,name),JSON.stringify(value,null,2),{flag:'wx',flush:true});}
function errorEvidence(error){return {message:String(error),stack:error?.stack||null};}

// FIX 1: Bounded, observational primary reads occur only before measurement and
// after policy-due ACKs. They do not repair files or substitute native authority.
function primaryCheckpoint(observations){
  const started=performance.now();let descriptor,observation;
  observations.attempts++;
  try{
    assert.ok(observations.attempts<=observations.maximumReads,'Checkpoint observation read budget exceeded');
    let file=root;
    for(const component of ['app-data','sessions','session.json']){
      file=path.join(file,component);
      assert.equal(fs.lstatSync(file).isSymbolicLink(),false,'Checkpoint namespace must not contain symbolic links');
    }
    const actual=owned(file);descriptor=fs.openSync(actual,'r');
    const before=fs.fstatSync(descriptor);
    assert.ok(before.isFile()&&Number.isSafeInteger(before.size)&&before.size>0&&before.size<=checkpointByteLimit,'Primary checkpoint exceeds the bounded native snapshot limit');
    const bytes=Buffer.alloc(before.size+1);let length=0;
    while(length<bytes.length){const read=fs.readSync(descriptor,bytes,length,bytes.length-length,null);if(!read)break;length+=read;observations.bytesRead+=read;}
    assert.equal(length,before.size,'Primary checkpoint changed size during observation');
    const after=fs.fstatSync(descriptor);
    assert.equal(after.size,before.size);assert.equal(after.mtimeMs,before.mtimeMs,'Primary checkpoint changed during observation');
    observation={path:path.relative(root,actual),...checkpointMetadata(bytes.subarray(0,length))};
    observations.successfulReads++;return observation;
  }finally{
    if(descriptor!==undefined)fs.closeSync(descriptor);
    const elapsedMs=performance.now()-started;
    observations.totalObservationMs+=elapsedMs;observations.maximumObservationMs=Math.max(observations.maximumObservationMs,elapsedMs);
    if(observation)observation.observationMs=elapsedMs;
  }
}

// FIX 1: Observe actual journal/checkpoint sizes without reading excluded lock
// files or following session evidence paths outside the disposable fixture.
function sessionStorage(){
  const directory=path.join(root,'app-data','sessions');
  if(!fs.existsSync(directory))return {directory:'app-data/sessions',available:false,files:[],totalBytes:null};
  owned(directory);
  const files=[];
  for(const entry of fs.readdirSync(directory,{withFileTypes:true})){
    assert.equal(entry.isSymbolicLink(),false,'Session evidence must not redirect outside the fixture');
    if(!entry.isFile()||entry.name.endsWith('.lock'))continue;
    const file=owned(path.join(directory,entry.name)),stat=fs.statSync(file);
    files.push({path:path.relative(root,file),sizeBytes:stat.size,
      kind:/^session-events-.*\.log$/.test(entry.name)?'event-journal':
        entry.name==='session.json'?'checkpoint':entry.name==='session.backup.json'?'checkpoint-backup':
        entry.name==='session-journal.authority.json'?'journal-authority':'other-session-file'});
  }
  files.sort((a,b)=>a.path.localeCompare(b.path));
  return {directory:'app-data/sessions',available:true,files,totalBytes:files.reduce((sum,file)=>sum+file.sizeBytes,0),
    journalBytes:files.filter(file=>file.kind==='event-journal').reduce((sum,file)=>sum+file.sizeBytes,0),
    checkpointBytes:files.filter(file=>file.kind==='checkpoint'||file.kind==='checkpoint-backup').reduce((sum,file)=>sum+file.sizeBytes,0)};
}

describe('Actual domain/native decision storage and restoration',()=>{
  it('retains 10,000 acknowledged decisions and measures exact restore and historical Undo',async function(){
    this.timeout(1800000);
    const samples=[],milestones=[];
    // FIX 1: Observe metadata only; transport retries can repeat side effects.
    assert.equal(browser.options.connectionRetryCount,0,'Capacity measurement requires automatic HTTP retries disabled');
    const transport=createDecisionTransport(browser,{maximumDecisions:workload.decisions});
    let attempts=0,acknowledged=0,prior=null,stage='setup',activeAttempt=null,failure=null,completed=false,restoreEvidence=null;
    // FIX 1: Only exact captured artifact configuration supplies the build label.
    let buildKind='unverified-e2e';
    let checkpoint=null,checkpointBaseline=null;
    const checkpointObservations={attempts:0,successfulReads:0,bytesRead:0,totalObservationMs:0,maximumObservationMs:0,
      maximumReads:1+Math.ceil(workload.decisions/checkpointInterval),maximumBytesPerRead:checkpointByteLimit,
      policy:'Native persist_admitted: acknowledged revision minus last checkpoint revision >= 50; not revision modulo 50.',
      limitation:'Checkpoint reads, allocation, parsing, hashing and evidence writes are outside ACK timing but perturb inter-command load and filesystem cache. Ordinary ACKs are policy-inferred; this does not prove the checkpoint never changed externally.'};
    const limitations=()=>[...workload.limitations.map(value=>value==='Debug/e2e artifact; not installed release'?`${buildKind} artifact; not installed release`:value),checkpointObservations.limitation];
    // FIX 2: Stop before the configured framework deadline, retaining uncertain
    // ownership. Catch/finally perform local evidence I/O only, never IPC cleanup.
    const deadlineAt=Date.now()+25*60*1000;
    let deadlineTimer;
    const deadline=new Promise((_,reject)=>{deadlineTimer=setTimeout(()=>{
      stopped=true;
      reject(new Error('DECISION_RUN_DEADLINE: action ownership unresolved; no further commands will be issued'));
    },25*60*1000);});
    const step=action=>{live();return Promise.race([Promise.resolve().then(action),deadline]);};
    const summary=()=>{
      const value=summarizeLatency(samples,{thresholdMs:workload.p95DurableCommandBudgetMs,minimumSamples:workload.minimumMeasuredSamples,
        metric:'durable-decision-ack',buildKind,boundaryDefinition:workload.boundary});
      if(acknowledged!==workload.decisions||activeAttempt||failure)value.observedThreshold='incomplete';
      return value;
    };
    const checkpointEvidence=()=>({baseline:checkpointBaseline,lastObserved:checkpoint,observations:{...checkpointObservations},
      subsets:checkpointSubsets(samples,{thresholdMs:workload.p95DurableCommandBudgetMs,minimumSamples:workload.minimumMeasuredSamples,
        metric:'durable-decision-ack',buildKind,boundaryDefinition:workload.boundary})});
    try{
      // FIX 3: Bind generated domain/workload/inventory to captured build bytes.
      const manifestFile=owned(path.join(root,'build-manifest.json'));
      const build=JSON.parse(fs.readFileSync(manifestFile,'utf8'));
      assert.ok(build.capturedBuild,'The sampler requires a source-captured native artifact');
      assert.equal(build.capturedBuild.binary.toLowerCase(),build.binarySha256.toLowerCase());
      buildKind=captureBuildKind(build.capturedBuild,build.binarySha256);
      // FIX 2: Keep the original workload/capture evidence; label observations
      // using the verified configuration, never an environment-provided label.
      record('decision-input-manifest.json',{
        evidenceClass:`single-exploratory-${buildKind}-decision-run`,buildKind,
        domainBundle:{path:path.relative(root,domainFile),sha256:digest(domainFile)},
        workload:{path:path.relative(root,workloadFile),sha256:digest(workloadFile),value:workload},
        inventory:{path:path.relative(root,inventoryFile),sha256:digest(inventoryFile),count:inventory.length,originals:inventory},
        capturedArtifact:{manifestPath:'build-manifest.json',manifestSha256:digest(manifestFile),binarySha256:build.binarySha256,build:build.capturedBuild},
        samplerSha256:digest(path.resolve('tests/desktop/decision-capacity.e2e.mjs')),
        helpers:['tests/qualification/decision-checkpoints.mjs','tests/qualification/decision-transport.mjs','tests/qualification/latency.mjs','tests/qualification/capture-release-e2e.mjs','tests/desktop/windows-paths.mjs'].map(file=>({path:file,sha256:digest(path.resolve(file))})),
        transportPolicy:{connectionRetryCount:0,observation:'Metadata-only request events; not wire capture; observer hashing/timing outside native ACK timing perturbs inter-command workload'},
        limitations:limitations(),checkpointObservationPolicy:{interval:checkpointInterval,maximumBytesPerRead:checkpointByteLimit,maximumReads:checkpointObservations.maximumReads},
      });
      originals();
      await step(()=>$('[aria-label="Help and keyboard shortcuts"]').waitForDisplayed());await step(saved);
      await step(()=>$('button=Add folder').click());
      await step(()=>browser.waitUntil(async()=>(await native('load_session'))?.report.images.length===workload.candidates,{timeout:120000}));
      await step(saved);await step(()=>browser.waitUntil(async()=>(await native('image_work_status')).active===0));
      let session=await step(()=>native('load_session'));
      const ids=session.report.images.slice().sort((a,b)=>a.path.localeCompare(b.path)).map(image=>image.id);
      session={...session,selected:ids,run:createRun(ids,'full-pairwise',19,false),review:null,screen:'arena',settings:{...session.settings,mode:'full-pairwise',randomize:false}};
      await step(()=>native('save_session',{value:session}));await step(saved);
      prior=(await step(()=>native('session_save_status'))).durableRevision;
      assert.ok(Number.isSafeInteger(prior));
      const decisionContext=await step(()=>native('preview_context'));
      checkpointBaseline=primaryCheckpoint(checkpointObservations);checkpoint=checkpointBaseline;
      assert.ok(checkpoint.revision<=prior&&prior-checkpoint.revision<checkpointInterval,'Saved baseline must have a current native checkpoint-policy position');
      stage='decisions';
      for(let index=0;index<workload.decisions;index++){
        live();
        const previousPair=session.run.pair;
        assert.ok(previousPair);
        session={...session,run:decide(session.run,previousPair.id,index%7===0?'both':'left')};
        attempts++;
        // FIX 2: Predict from the last observed checkpoint; verify every due ACK
        // against primary bytes after the timed native command has returned.
        activeAttempt={index,actionId:previousPair.id,expectedRevision:prior+1,checkpoint:checkpointExpectation(checkpoint.revision,prior,prior+1)};
        // FIX 2: Bind the logical decision before dispatch; no payload archive.
        transport.begin(activeAttempt);
        const {result}=await step(()=>browser.executeAsync((value,contextId,cutoff,done)=>{
          void(async()=>{
            const started=performance.now();let timer;
            try{
              const remaining=Math.min(25000,cutoff-Date.now());
              if(remaining<=0)return {ok:false,outcome:'timeout',elapsedMs:0,error:'RUN_DEADLINE: no native save admitted'};
              const acknowledgment=await Promise.race([
                window.__TAURI__.core.invoke('save_session',{contextId,value}).then(()=>({saved:true})),
                new Promise(resolve=>{timer=setTimeout(()=>resolve({saved:false}),remaining);}),
              ]);
              const elapsedMs=performance.now()-started;
              if(!acknowledgment.saved)return {ok:false,outcome:'timeout',elapsedMs,error:'SAVE_ACK_TIMEOUT: native ownership unresolved'};
              if(Date.now()>=cutoff)return {ok:false,outcome:'timeout',elapsedMs,error:'RUN_DEADLINE: saved return observed; revision unverified'};
              const status=await window.__TAURI__.core.invoke('session_save_status',{contextId});
              return {ok:true,elapsedMs,status};
            }catch(error){return {ok:false,outcome:'error',elapsedMs:performance.now()-started,error:String(error)};}
            finally{clearTimeout(timer);}
          })().then(result=>done({result}),error=>done({result:{ok:false,outcome:'error',error:String(error)}}));
        },session,decisionContext,deadlineAt));
        const sample={actionId:previousPair.id,phase:'measure',outcome:result.ok?'success':result.outcome||'error',elapsedMs:result.elapsedMs??null,checkpoint:{...activeAttempt.checkpoint}};
        samples.push(sample);
        if(!result.ok){sample.reason=result.error;transport.finish('native-rejected-or-timeout');throw new Error(result.error);}
        try{
          assert.equal(result.status.state,'saved');
          assert.equal(result.status.durableRevision,prior+1,'Every acknowledged decision must increment the exact native durable revision once');
          prior=result.status.durableRevision;acknowledged++;activeAttempt=null;
          transport.finish('native-ack-verified');
          if(sample.checkpoint.expected){
            const current=primaryCheckpoint(checkpointObservations);
            sample.checkpoint.after=current.revision;
            sample.checkpoint.observationMs=current.observationMs;
            sample.checkpoint.bytes=current.bytes;sample.checkpoint.sha256=current.sha256;
            Object.assign(sample.checkpoint,verifyCheckpointChange(checkpoint,current,prior));
            checkpoint=current;
          }
        }catch(error){sample.outcome='rejected';sample.reason=String(error);throw error;}
        if([1000,5000,10000].includes(index+1)){
          const loaded=await step(()=>native('load_session'));assert.deepEqual(loaded.run,session.run);
          milestones.push({decisions:index+1,durableRevision:prior,uiJsonBytes:Buffer.byteLength(JSON.stringify(loaded))});
        }
        if((index+1)%100===0){
          record(`decision-progress-${String(index+1).padStart(5,'0')}.json`,{
            status:'in-progress',attempts,acknowledged,durableRevision:prior,summary:summary(),samples,milestones,storage:sessionStorage(),checkpointEvidence:checkpointEvidence(),transport:transport.snapshot(),limitations:limitations(),
          });
          console.log(`Acknowledged ${index+1} actual domain/native decisions`);
        }
      }
      assert.equal(acknowledged,workload.decisions);assertValidRun(session.run);
      // FIX 3: Keep overall samples/p95 intact, with disjoint observed/inferred
      // subsets and overhead attached to every progress/final evidence record.
      record('decision-latency.json',{workload,attempts,acknowledged,samples,summary:summary(),milestones,storage:sessionStorage(),checkpointEvidence:checkpointEvidence(),transport:transport.snapshot()});
      stage='renderer-restore';
      const beforeContext=await step(()=>native('preview_context')),restoreStarted=performance.now();
      await step(()=>browser.refresh());
      await step(()=>$('button*=Prefer left').waitForEnabled({timeout:120000}));await step(saved);
      const restoreReadyMs=performance.now()-restoreStarted;
      const restored=await step(()=>native('load_session'));
      assert.deepEqual(restored.run,session.run);assertValidRun(restored.run);
      assert.notEqual(await step(()=>native('preview_context')),beforeContext);
      assert.equal((await step(()=>native('session_save_status'))).durableRevision,prior,'Renderer reload alone must not append a decision');
      stage='historical-undo';
      const expectedUndo=undo(restored.run);
      // FIX 4: Arena's actual native button includes its visible Ctrl Z hint.
      // Verify the label, then wait for and compare the complete native result.
      const undoButton=await $('.arena-heading button*=Undo');
      assert.equal((await step(()=>undoButton.getText())).replace(/\s+/g,' ').trim(),'Undo Ctrl Z');
      await step(()=>undoButton.waitForEnabled());await step(()=>undoButton.click());
      await step(()=>browser.waitUntil(async()=>(await native('load_session')).run.history.length===workload.decisions-1));
      await step(saved);
      const afterUndo=await step(()=>native('load_session'));assert.deepEqual(afterUndo.run,expectedUndo);assertValidRun(afterUndo.run);
      const undoStatus=await step(()=>native('session_save_status'));
      assert.equal(undoStatus.state,'saved');assert.equal(undoStatus.durableRevision,prior+1);
      originals();
      restoreEvidence={workload,restoreReadyMs,restoreThresholdMet:restoreReadyMs<workload.restoreReadyBudgetMs,
        durableThresholdMet:summary().observedThreshold==='met',exactRunRestored:true,exactHistoricalUndo:true,
        acknowledgedDecisionRevision:prior,undoDurableRevision:undoStatus.durableRevision,buildKind,limitations:limitations()};
      record('decision-restore.json',restoreEvidence);
      completed=true;stage='completed';
    }catch(error){
      stopped=true;failure=error;
      transport.interrupt(error);
      if(activeAttempt&&!samples.some(sample=>sample.actionId===activeAttempt.actionId))
        samples.push({actionId:activeAttempt.actionId,phase:'measure',outcome:'ownership-unresolved',elapsedMs:null,reason:String(error),checkpoint:{...activeAttempt.checkpoint}});
      throw error;
    }finally{
      clearTimeout(deadlineTimer);stopped=true;
      // FIX 3: Detach before framework cleanup; no after-failure IPC or retry.
      transport.dispose();
      const localErrors=[];let originalBytesVerified=false,storage=null;
      try{originals();originalBytesVerified=true;}catch(error){localErrors.push({step:'original byte verification',...errorEvidence(error)});}
      try{storage=sessionStorage();}catch(error){localErrors.push({step:'session file size evidence',...errorEvidence(error)});}
      const complete=completed&&!failure&&!localErrors.length&&acknowledged===workload.decisions;
      const value={status:complete?'completed':'interrupted',stage,attempts,acknowledged,requiredDecisions:workload.decisions,
        lastVerifiedDurableRevision:prior,activeAttempt,error:failure?errorEvidence(failure):null,localErrors,
        summary:summary(),samples,milestones,storage,originalBytesVerified,restoreEvidence,checkpointEvidence:checkpointEvidence(),transport:transport.snapshot(),
        // FIX 3: Optimized e2e measurements still do not qualify an installed release.
        qualification:`single exploratory ${buildKind} run only; no 24 MP, installed-release, or full-repeat qualification`,limitations:limitations()};
      if(!complete)value.summary.observedThreshold='incomplete';
      try{record(complete?'decision-final.json':'decision-interrupted.json',value);}
      catch(error){localErrors.push({step:'write final evidence',...errorEvidence(error)});}
      if(localErrors.length){
        console.error('Decision sampler local evidence failure:',JSON.stringify(localErrors));
        if(!failure)throw new AggregateError(localErrors.map(error=>new Error(`${error.step}: ${error.message}`)),'Decision sampler evidence is incomplete');
      }
      // No browser/native command here: an uncertain save remains owned until
      // the parent performs verified process cleanup or outcome reconciliation.
    }
  });
});
