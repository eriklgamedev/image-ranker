// The optimized capacity route inherits the normal driver's supervision and
// afterTest evidence. Importing this config is an execution step, not a pure test.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {hashFile,releaseRelativePath,validateReleaseCapture} from './tests/qualification/capture-release-e2e.mjs';
import {desktopProfile} from './tests/qualification/desktop-profile.mjs';

const workspace=path.dirname(fileURLToPath(import.meta.url));
assert.equal(process.platform,'win32','Optimized capacity is a Windows x64 qualification route');
assert.equal(process.arch,'x64');
const qualificationProfile=desktopProfile(process.env.IMAGE_RANKER_QUALIFICATION_PROFILE,process.argv.slice(2));
assert.equal(qualificationProfile.name,'decision-capacity','Set the explicit decision-capacity profile; other specs cannot use this route');
const capturePath=path.resolve(workspace,process.env.IMAGE_RANKER_RELEASE_E2E_CAPTURE||'tests-artifacts/e2e-release-build-current.json');
const readCapture=()=>JSON.parse(fs.readFileSync(capturePath,'utf8').replace(/^\uFEFF/,''));
const binary=path.resolve(workspace,releaseRelativePath);
validateReleaseCapture(readCapture(),{root:workspace,binaryHash:hashFile(binary)});
// Import only after qualification metadata is accepted: base config may create
// a disposable fixture and must not do so for an invalid release invocation.
const {config:base}=await import('./wdio.conf.mjs');
const root=process.env.IMAGE_RANKER_E2E_ROOT;
assert.ok(root&&path.isAbsolute(root));
assert.equal(base.capabilities.length,1);assert.equal(base.services.length,1);
const extraInputs=['wdio.capacity-release.conf.mjs','tests/qualification/capture-release-e2e.mjs',
  'tests/qualification/latency.mjs','tests/qualification/decision-checkpoints.mjs','tests/qualification/decision-transport.mjs'];

export const config={...base,
  connectionRetryCount:0,
  capabilities:base.capabilities.map(capability=>({...capability,'tauri:options':{...capability['tauri:options'],application:binary}})),
  services:base.services.map(([service,options])=>[service,{...options,appBinaryPath:binary}]),
  onPrepare(){
    const captured=readCapture(),binarySha256=hashFile(binary);
    validateReleaseCapture(captured,{root:workspace,binaryHash:binarySha256});
    const sameHarness=[...fs.readdirSync(path.join(workspace,'tests/desktop')).filter(file=>file.endsWith('.e2e.mjs')).sort().map(file=>`tests/desktop/${file}`),
      'tests/desktop/windows-paths.mjs','tests/desktop/windows-driver.mjs','tests/desktop/kill-app.ps1','tests/desktop/fixtures.mjs','tests/qualification/desktop-profile.mjs','wdio.conf.mjs'];
    const tests=[...new Set([...sameHarness,...extraInputs])].map(file=>({path:file,sha256:hashFile(path.join(workspace,file))}));
    fs.writeFileSync(path.join(root,'build-manifest.json'),JSON.stringify({binarySha256,capturedBuild:captured,qualificationProfile,
      releaseCapture:{path:capturePath,sha256:hashFile(capturePath)},buildKind:'optimized-e2e',installedReleaseQualified:false,
      transport:{connectionRetryCount:0,requestObservation:'metadata-only WDIO events; no automatic HTTP replay'},tests},null,2),{flag:'wx',flush:true});
    console.log(`Disposable optimized e2e capacity fixture: ${root}`);
  },
};
