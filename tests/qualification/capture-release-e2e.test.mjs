import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {captureBuildKind,enumerateAppInputs,validateReleaseCapture} from './capture-release-e2e.mjs';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'../..');
const hash='a'.repeat(64);
const sources=[{path:'src/App.tsx',sha256:'b'.repeat(64)}];
const captured={binary:hash,binaryPath:path.resolve(root,'src-tauri/target/release/image-ranker.exe'),feature:'e2e',configuration:'release/no-bundle',
  platform:'win32',architecture:'x64',sources,sourceTreeSha256:crypto.createHash('sha256').update(JSON.stringify(sources)).digest('hex')};
test('capacity labels derive only from matching captured feature/configuration and artifact hash',()=>{
  assert.equal(captureBuildKind(captured,hash),'optimized-e2e');
  assert.equal(captureBuildKind({...captured,configuration:'debug/no-bundle'},hash),'debug-e2e');
  assert.throws(()=>captureBuildKind({...captured,feature:'production'},hash),/actual e2e/);
  assert.throws(()=>captureBuildKind({...captured,configuration:'installed-release'},hash),/Unqualified/);
  assert.throws(()=>captureBuildKind(captured,'c'.repeat(64)));
});
test('release path/configuration/source digest are exact; a different directory or leaf cannot qualify',()=>{
  // Windows fixture paths are validated on the actual Windows qualification host.
  if(process.platform!=='win32')return;
  const options={root,binaryHash:hash,verifySources:false};
  assert.equal(validateReleaseCapture(captured,options).installedReleaseQualified,false);
  for(const change of [{binaryPath:path.resolve(root,'src-tauri/target/debug/image-ranker.exe')},
    {binaryPath:path.resolve(root,'src-tauri/target/release/another.exe')},{configuration:'debug/no-bundle'},
    {sourceTreeSha256:'0'.repeat(64)},{architecture:'arm64'}])assert.throws(()=>validateReleaseCapture({...captured,...change},options));
});
test('application capture includes every current source, icon, asset and separate lease input, excluding generated targets',()=>{
  const current=enumerateAppInputs(root);
  // Validate actual source coverage without requiring a private historical build.
  for(const directory of ['src','src-tauri/src','src-tauri/capabilities','src-tauri/icons','assets'])
    for(const file of fs.readdirSync(path.join(root,directory),{recursive:true}).filter(file=>fs.statSync(path.join(root,directory,file)).isFile()))
      assert.ok(current.includes(directory+'/'+file.replaceAll('\\','/')),'New application input missing: '+file);
  assert.ok(current.includes('src-tauri/install-lifetime-core/Cargo.toml'));
  for(const file of fs.readdirSync(path.join(root,'src-tauri/install-lifetime-core/src'),{recursive:true}).filter(file=>file.endsWith('.rs')))
    assert.ok(current.includes('src-tauri/install-lifetime-core/src/'+file.replaceAll('\\','/')));
  assert.equal(current.some(file=>file.includes('/target/')),false);
});
