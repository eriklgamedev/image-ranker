import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';

const fixture=path.resolve(process.argv[2]||'');
const expected=Number(process.argv[3]);
const workspace=path.resolve('.');
const artifacts=path.join(workspace,'tests-artifacts');
if(!fixture.startsWith(artifacts+path.sep)||!Number.isInteger(expected)||expected<1)
  throw new Error('Supply this workspace’s disposable fixture and expected case count');
const read=file=>JSON.parse(fs.readFileSync(file,'utf8').replace(/^\uFEFF/,''));
const digest=file=>crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const manifest=read(path.join(fixture,'build-manifest.json'));
const captured=manifest.capturedBuild;
if(!captured||captured.binary!==manifest.binarySha256)throw new Error('Missing exact captured build');
if(digest(captured.binaryPath)!==manifest.binarySha256)throw new Error('Executable changed after run');
for(const test of manifest.tests)if(digest(test.path)!==test.sha256)throw new Error(`Harness changed after run: ${test.path}`);
const results=fs.readFileSync(path.join(fixture,'test-results.jsonl'),'utf8').trim().split('\n').map(JSON.parse);
if(results.length!==expected||results.some(item=>!item.passed))throw new Error('Missing or failing cases');
const changedSources=captured.sources.filter(input=>!fs.existsSync(input.path)||digest(input.path)!==input.sha256).map(input=>input.path);
const record={createdAt:new Date().toISOString(),fixture,binarySha256:manifest.binarySha256,
  configuration:captured.configuration,feature:captured.feature,cases:results.length,passed:results.length,
  binaryAndHarnessUnchanged:true,changedSourcesSinceCapturedBuild:changedSources,
  qualification:'Captured e2e binary in the recorded build configuration and exact harness only; later source changes and installed release are not qualified by this run.'};
fs.writeFileSync(path.join(fixture,'qualification-verification.json'),JSON.stringify(record,null,2)+'\n',{flag:'wx',flush:true});
console.log(JSON.stringify(record));
