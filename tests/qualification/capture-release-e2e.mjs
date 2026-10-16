// Capture only: this never builds or launches the app. The coordinator must
// independently freeze/verify these inputs around the actual optimized build.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import crypto from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {execFileSync} from 'node:child_process';
import {windowsLocationKey} from '../desktop/windows-paths.mjs';

export const releaseRelativePath='src-tauri/target/release/image-ranker.exe';
const inputDirectories=['src','src-tauri/src','src-tauri/capabilities','src-tauri/icons','src-tauri/install-lifetime-core/src','assets'];
const inputFiles=['index.html','package.json','package-lock.json','vite.config.ts','tsconfig.json','src-tauri/build.rs',
  'rust-toolchain.toml','src-tauri/Cargo.toml','src-tauri/Cargo.lock','src-tauri/install-lifetime-core/Cargo.toml','src-tauri/tauri.conf.json','src-tauri/tauri.e2e.conf.json'];
const sha=bytes=>crypto.createHash('sha256').update(bytes).digest('hex');
function walk(root,directory) {
  const absolute=path.join(root,directory);
  assert.equal(fs.lstatSync(absolute).isSymbolicLink(),false,'Linked application inputs are not qualified');
  return fs.readdirSync(absolute,{withFileTypes:true}).flatMap(entry=>{
    assert.equal(entry.isSymbolicLink(),false,'Linked application inputs are not qualified');
    const relative=path.join(directory,entry.name);
    return entry.isDirectory()?walk(root,relative):entry.isFile()?[relative]:[];
  });
}
export function enumerateAppInputs(root) {
  // Deliberately identical application/config/icon/license coverage to capture-build.mjs.
  const optional=['src-tauri/install-lifetime-core/Cargo.lock','src-tauri/install-lifetime-core/build.rs','src-tauri/install-lifetime-core/.cargo/config.toml'].filter(file=>fs.existsSync(path.join(root,file)));
  return [...inputDirectories.flatMap(directory=>walk(root,directory)),...inputFiles,...optional].sort().map(file=>file.replaceAll('\\','/'));
}
export function hashFile(file) {
  const descriptor=fs.openSync(file,'r'),hash=crypto.createHash('sha256'),buffer=Buffer.alloc(1024*1024);
  try {let count;while((count=fs.readSync(descriptor,buffer,0,buffer.length,null))>0)hash.update(buffer.subarray(0,count));return hash.digest('hex');}
  finally {fs.closeSync(descriptor);}
}
export function sourcesAt(root) {
  return enumerateAppInputs(root).map(relative=>({path:relative,sha256:hashFile(path.join(root,relative))}));
}
export function captureBuildKind(captured,observedHash) {
  assert.ok(captured,'The sampler requires a source-captured native artifact');
  assert.equal(captured.feature,'e2e','Capacity uses the actual e2e adapter, never a production/install label');
  assert.match(observedHash,/^[0-9a-f]{64}$/i);assert.equal(captured.binary.toLowerCase(),observedHash.toLowerCase());
  if(captured.configuration==='debug/no-bundle')return 'debug-e2e';
  if(captured.configuration==='release/no-bundle')return 'optimized-e2e';
  throw new Error('Unqualified capacity build configuration');
}
export function validateReleaseCapture(captured,{root,binaryHash,platform=process.platform,architecture=process.arch,verifySources=true}) {
  assert.equal(platform,'win32');assert.equal(architecture,'x64');
  assert.equal(captured.platform,'win32');assert.equal(captured.architecture,'x64');
  assert.equal(captureBuildKind(captured,binaryHash),'optimized-e2e');
  const expected=path.resolve(root,releaseRelativePath);
  assert.equal(windowsLocationKey(captured.binaryPath),windowsLocationKey(expected),'Capture must bind the exact release executable');
  assert.ok(Array.isArray(captured.sources)&&captured.sources.length>0,'Captured source inventory is required');
  assert.equal(captured.sourceTreeSha256,sha(JSON.stringify(captured.sources)),'Captured source inventory digest mismatch');
  if(verifySources)assert.deepEqual(sourcesAt(root),captured.sources,'Application inputs changed after the release capture');
  return {binary:expected,buildKind:'optimized-e2e',installedReleaseQualified:false};
}
export function captureReleaseE2e({root,output,versions}) {
  assert.equal(process.platform,'win32','The release qualification targets Windows x64');assert.equal(process.arch,'x64');
  const binary=path.resolve(root,releaseRelativePath),sources=sourcesAt(root);
  const info=versions??(()=>{
    const options={encoding:'utf8',windowsHide:true,cwd:root,env:{...process.env,RUSTUP_TOOLCHAIN:'stable',CARGO_HOME:path.join(root,'.cargo-home')}};
    return {rustc:execFileSync('rustc',['--version'],options).trim(),cargo:execFileSync('cargo',['--version'],options).trim()};
  })();
  const record={createdAt:new Date().toISOString(),binary:hashFile(binary),binaryPath:binary,
    platform:process.platform,architecture:process.arch,osRelease:os.release(),node:process.version,...info,
    feature:'e2e',configuration:'release/no-bundle',sources,sourceTreeSha256:sha(JSON.stringify(sources)),
    qualification:'Optimized instrumentation build only; not an installed production package. Coordinator build input freeze/verification remains separate.'};
  fs.mkdirSync(path.dirname(output),{recursive:true});
  fs.writeFileSync(output,JSON.stringify(record,null,2)+'\n',{flag:'wx',flush:true});
  return record;
}
if(process.argv[1]&&path.resolve(process.argv[1])===fileURLToPath(import.meta.url)) {
  const args=process.argv.slice(2);
  assert.ok(args.length===0||(args.length===2&&args[0]==='--output'),'Usage: node capture-release-e2e.mjs [--output fresh-path.json]');
  const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'../..');
  const output=path.resolve(root,args[1]??'tests-artifacts/e2e-release-build-current.json');
  const record=captureReleaseE2e({root,output});
  console.log(`Captured optimized e2e binary ${record.binary} and ${record.sources.length} application inputs: ${output}`);
}
