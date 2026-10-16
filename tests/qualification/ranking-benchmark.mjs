import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import crypto from 'node:crypto';
import {build} from 'esbuild';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';

const root=fs.mkdtempSync(path.resolve('tests-artifacts/ranking-benchmark-'));
const inputs=fs.readdirSync('src/domain/ranking').filter(name=>name.endsWith('.ts')).sort().map(name=>{
  const file=`src/domain/ranking/${name}`;
  return {path:file,sha256:crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex')};
});
const meta={createdAt:new Date().toISOString(),node:process.version,cpu:os.cpus()[0]?.model,logicalCpus:os.cpus().length,
  totalMemoryBytes:os.totalmem(),osRelease:os.release(),inputs,
  qualification:'Actual domain replay only; excludes native storage, IPC, image validation, UI hydration and presentation. No installed-release or whole-app restore pass.'};
fs.writeFileSync(path.join(root,'inputs.json'),JSON.stringify(meta,null,2),{flag:'wx'});
const entry=`import assert from 'node:assert/strict';
import fs from 'node:fs';
import {createRun,decide,assertValidRun,undo} from './src/domain/ranking/index.ts';
const results=[];
for(let repeat=0;repeat<5;repeat++){
  let run=createRun(Array.from({length:143},(_,i)=>'image-'+i),'full-pairwise',19,false);
  const started=performance.now();
  for(let i=0;i<10000;i++)run=decide(run,run.pair.id,i%7===0?'both':'left');
  const setupMs=performance.now()-started,serialized=JSON.stringify(run);
  const replayStarted=performance.now(),restored=JSON.parse(serialized);
  assertValidRun(restored);
  const replayMs=performance.now()-replayStarted;
  assert.deepEqual(restored,run);assert.equal(restored.history.length,10000);assert.ok(restored.pair);
  const undoStarted=performance.now(),undone=undo(restored),undoMs=performance.now()-undoStarted;
  assert.equal(undone.history.length,9999);assert.notEqual(undone.pair.id,run.pair.id);
  assertValidRun(undone);
  const result={repeat,decisions:10000,candidates:143,setupMs,parseAndFullValidationMs:replayMs,undoMs,jsonBytes:Buffer.byteLength(serialized),processSnapshot:process.memoryUsage(),domainOnlyBelow2000ms:replayMs<2000};
  results.push(result);console.log(JSON.stringify(result));
  fs.writeFileSync(process.argv[2],JSON.stringify({results},null,2));
}
`;
await build({stdin:{contents:entry,resolveDir:path.resolve('.'),sourcefile:'ranking-qualified-benchmark.ts',loader:'ts'},bundle:true,platform:'node',format:'esm',outfile:path.join(root,'run.mjs')});
const child=execFile(process.execPath,[path.join(root,'run.mjs'),path.join(root,'results.json')],{windowsHide:true,timeout:300000,maxBuffer:2*1024*1024});
child.stdout.pipe(process.stdout);child.stderr.pipe(process.stderr);
await new Promise((resolve,reject)=>{child.on('error',reject);child.on('exit',code=>code===0?resolve():reject(new Error(`Benchmark exited ${code}; evidence ${root}`)));});
for(const input of inputs)if(crypto.createHash('sha256').update(fs.readFileSync(input.path)).digest('hex')!==input.sha256)throw new Error(`Source changed: ${input.path}`);
fs.writeFileSync(path.join(root,'verification.json'),JSON.stringify({...meta,inputsUnchanged:true,results:JSON.parse(fs.readFileSync(path.join(root,'results.json'),'utf8')).results},null,2),{flag:'wx',flush:true});
console.log(`Domain benchmark evidence: ${root}`);
