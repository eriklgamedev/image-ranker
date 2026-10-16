import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {captureFiles, compareManifests, assertProductionFeatures, collectOutputs, collectInstallerEvidence, installerInputs, peMachine, writeNew, classifyFailure, target} from './package-capture.mjs';

function fixture(t) {
  const root=fs.mkdtempSync(path.join(os.tmpdir(),'ranker-package-model-'));
  t.after(()=>{
    assert.equal(path.dirname(path.resolve(root)),path.resolve(os.tmpdir()));
    assert.match(path.basename(root),/^ranker-package-model-/);
    fs.rmSync(root,{recursive:true,force:true});
  });
  return root;
}
function write(root,name,bytes='fixture') {
  const file=path.join(root,name);fs.mkdirSync(path.dirname(file),{recursive:true});fs.writeFileSync(file,bytes);return file;
}
function feature(name,features='[]') {
  return {path:`${target}/release/.fingerprint/image-ranker-abc/${name}.json`,value:{features}};
}
const normal=[feature('lib-image_ranker_lib'),feature('bin-image-ranker')];
test('input verification detects addition, removal and content changes, not just old file hashes',()=>{
  const result=compareManifests({treeSha256:'before',inputs:[{path:'a',sha256:'1',bytes:1},{path:'b',sha256:'2',bytes:1}]},
    {treeSha256:'after',inputs:[{path:'a',sha256:'9',bytes:1},{path:'c',sha256:'2',bytes:1}]});
  assert.equal(result.unchanged,false);assert.deepEqual(result.changes,[{path:'a',kind:'changed'},{path:'b',kind:'removed'},{path:'c',kind:'added'}]);
});
test('same manifest passes and empty signature data is never inferred',()=>{
  const record={treeSha256:'A',inputs:[{path:'app',sha256:'hash',bytes:4}]};
  assert.equal(compareManifests(record,record).unchanged,true);
});
test('root e2e/default features and any built automation dependency fail production evidence',()=>{
  assert.equal(assertProductionFeatures(normal).automationDependenciesFound,false);
  for(const invalid of ['e2e','default'])assert.throws(()=>assertProductionFeatures([normal[0],feature('bin-image-ranker',JSON.stringify([invalid]))]),/e2e or default/);
  assert.throws(()=>assertProductionFeatures([...normal,{path:'release/.fingerprint/tauri-plugin-wdio-xyz/lib-tauri_plugin_wdio.json',value:{features:'[]'}}]),/Automation dependency/);
});
test('missing or unknown Cargo feature schema fails closed',()=>{
  assert.throws(()=>assertProductionFeatures([normal[0]]),/complete release/);
  assert.throws(()=>assertProductionFeatures([normal[0],feature('bin-image-ranker','{}')]),/Unknown Cargo/);
});
test('capture never overwrites an earlier evidence file',t=>{
  const root=fixture(t),file=path.join(root,'capture.json');writeNew(file,{first:true});
  assert.throws(()=>writeNew(file,{second:true}),/EEXIST/);assert.deepEqual(JSON.parse(fs.readFileSync(file)),{first:true});
});
test('no artifacts produced is recorded as absence, without install/signing claims',t=>{
  const root=fixture(t),evidence=path.join(root,'evidence'),targetRoot=path.join(root,'target');fs.mkdirSync(evidence);
  const result=collectOutputs(root,targetRoot,evidence);
  assert.deepEqual(result.artifacts,[]);assert.equal(result.installed,false);assert.equal(result.signedReleaseQualified,false);
  assert.match(result.featureError,/No complete release/);
});
test('only app and NSIS output in this fresh target are recorded with exact bytes; dist is separate',t=>{
  const root=fixture(t),evidence=path.join(root,'evidence'),targetRoot=path.join(root,'target');fs.mkdirSync(evidence);
  write(root,'dist/index.html','fresh frontend');
  write(targetRoot,`${target}/release/image-ranker.exe`,'app bytes');
  write(targetRoot,`${target}/release/bundle/nsis/Image Ranker.exe`,'installer bytes');
  write(root,'old-target/image-ranker.exe','old bytes');
  for(const entry of normal)write(targetRoot,entry.path,JSON.stringify(entry.value));
  const result=collectOutputs(root,targetRoot,evidence);
  assert.deepEqual(result.artifacts.map(item=>[item.kind,item.bytes]),[['application',9],['nsis-installer',15]]);
  assert.equal(result.generatedFrontend[0].path,'dist/index.html');assert.equal(result.featureError,null);
  const retained=JSON.parse(fs.readFileSync(path.join(evidence,'cargo-fingerprints.json')));
  assert.equal(retained.records.length,2);assert.equal(retained.records[0].raw,'{"features":"[]"}');
});
test('lossless raw fingerprints preserve integer identity fields rather than JS rounded reserialization',t=>{
  const root=fixture(t),evidence=path.join(root,'evidence'),targetRoot=path.join(root,'target');fs.mkdirSync(evidence);
  const raw='{"rustc":18446744073709551615,"features":"[]","deps":[[18446744073709551614,"tauri",false,18446744073709551613]]}';
  for(const entry of normal)write(targetRoot,entry.path,raw);
  collectOutputs(root,targetRoot,evidence);
  const result=JSON.parse(fs.readFileSync(path.join(evidence,'cargo-fingerprints.json')));
  assert.equal(result.records[0].raw,raw);
});
test('input path outside root is refused without reading it',t=>{
  const root=fixture(t),outside=fixture(t),file=write(outside,'external','secret');
  assert.throws(()=>captureFiles(root,[file]),/outside/);
});
test('failure hints retain category uncertainty instead of claiming a repaired dependency or installer',()=>{
  const result=classifyFailure('NSIS failed to download WebView2: DNS connection failed');
  assert.deepEqual(result.categories,['dependency-unavailable','windows-bundler-or-runtime','download-or-network']);
  assert.match(result.classification,/exact stdout\/stderr/);
  assert.deepEqual(classifyFailure('unknown exception').categories,['unclassified-command-failure']);
});
test('installer input inventory includes shared core/plugin locks and original includes but excludes generated target trees',t=>{
  const root=fixture(t);
  const expected=['src-tauri/install-lifetime-core/Cargo.toml','src-tauri/install-lifetime-core/Cargo.lock','src-tauri/install-lifetime-core/src/lib.rs',
    'installer/nsis-plugin/Cargo.toml','installer/nsis-plugin/Cargo.lock','installer/nsis-plugin/src/lib.rs','installer/nsis-plugin/upstream/pluginapi.h',
    'installer/nsis-plugin/artifacts/ImageRankerLifetime.dll','installer/nsis/lifetime.nsh','installer/nsis/upstream/LICENSE-MIT'];
  for(const file of [...expected,'installer/nsis-plugin/target/dependency.dll','src-tauri/install-lifetime-core/target/generated.rs'])write(root,file);
  assert.deepEqual(installerInputs(root).map(file=>path.relative(root,file).replaceAll('\\','/')).sort(),expected.sort());
});
test('exact rendered include bytes, local tool bytes and actual i686 plugin header are captured without installed claims',t=>{
  const root=fixture(t),targetRoot=path.join(root,'target'),evidence=path.join(root,'evidence');fs.mkdirSync(evidence);
  const raw=Buffer.from('\ufeff; exact local rendered script\r\n');
  write(targetRoot,`${target}/release/nsis/x64/installer.nsi`,raw);
  write(targetRoot,`${target}/release/nsis/x64/English.nsh`,'language include');
  write(targetRoot,'.tauri/NSIS/makensis.exe','actual tool bytes');write(targetRoot,'.tauri/NSIS/Include/MUI2.nsh','actual tool include');
  const pe=Buffer.alloc(70);pe.writeUInt16LE(0x5a4d,0);pe.writeUInt32LE(64,60);pe.writeUInt32LE(0x4550,64);pe.writeUInt16LE(0x14c,68);
  write(root,'installer/nsis-plugin/artifacts/ImageRankerLifetime.dll',pe);
  const result=collectInstallerEvidence(root,targetRoot,evidence);assert.deepEqual(result.issues,[]);assert.equal(result.plugin.machine,0x14c);
  assert.equal(result.tools.length,2);assert.equal(result.rendered.length,2);
  assert.deepEqual(fs.readFileSync(path.join(evidence,'rendered-installer',`${target}/release/nsis/x64/installer.nsi`)),raw);
  assert.match(result.scope,/no installation/);assert.throws(()=>collectInstallerEvidence(root,targetRoot,evidence),/EEXIST/);
});
test('missing tool outputs and wrong plugin architecture stay explicit and never become qualification success',t=>{
  const root=fixture(t),targetRoot=path.join(root,'target'),evidence=path.join(root,'evidence');fs.mkdirSync(evidence);
  const file=write(root,'installer/nsis-plugin/artifacts/ImageRankerLifetime.dll','not a PE');assert.throws(()=>peMachine(file),/Invalid PE/);
  let result=collectInstallerEvidence(root,targetRoot,evidence);assert.equal(result.issues.length,3);
  const pe=Buffer.alloc(70);pe.writeUInt16LE(0x5a4d,0);pe.writeUInt32LE(64,60);pe.writeUInt32LE(0x4550,64);pe.writeUInt16LE(0x8664,68);fs.writeFileSync(file,pe);
  result=collectInstallerEvidence(root,targetRoot,evidence);assert.ok(result.issues.some(issue=>issue.includes('not i686')));
});
