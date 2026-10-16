import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {fileURLToPath} from 'node:url';
const root=path.dirname(fileURLToPath(import.meta.url));
const text=fs.readFileSync(path.join(root,'installer.nsi'),'utf8');
const utils=fs.readFileSync(path.join(root,'image-ranker-utils.nsh'),'utf8');
test('pinned upstream templates and notices remain byte-identical',()=>{
  const expected={'installer.nsi':'dabed59013b1d78b879a1a85bc7f2eed2993b33a9a90cdabe5946de3d3950597','utils.nsh':'78272bd40b8a783c0ab94e4bcb400fd947ba306a7575f2bbbbccb7478f40447f',
    'LICENSE-APACHE-2.0':'0d542e0c8804e39aa7f37eb00da5a762149dc682d7829451287e11b938e94594','LICENSE-MIT':'9dd42ea92cff2ede5cd477cbfcce051b2d0115c0ac7f368ee88cb545055dff1d'};
  for(const[file,sha]of Object.entries(expected))assert.equal(crypto.createHash('sha256').update(fs.readFileSync(path.join(root,'upstream',file))).digest('hex'),sha);
  const manifest=JSON.parse(fs.readFileSync(path.join(root,'upstream/source-manifest.json'),'utf8'));
  for(const item of manifest.files){const bytes=fs.readFileSync(path.join(root,'upstream',item.path));assert.equal(bytes.length,item.bytes);assert.equal(crypto.createHash('sha256').update(bytes).digest('hex'),item.sha256);}
});
test('downstream paths cannot force-close apps, execute legacy uninstallers, or delete app data',()=>{
  for(const forbidden of [/RmShutdown/i,/RmForceShutdown/i,/RestartManager_Shutdown/i,/taskkill/i,/TerminateProcess/i,/CheckIfAppIsRunning/,/PageReinstall/,/PageLeaveReinstall/,/ExecWait[^\n]*UninstallString/i,/DeleteAppData/,/RmDir\s+\/r\b/i,/\/REBOOTOK/i])assert.doesNotMatch(text+'\n'+utils,forbidden);
  assert.doesNotMatch(text,/ExecWait[^\n]*\$R1.*\/UPDATE/);assert.doesNotMatch(text,/Delete "\$INSTDIR/);
  assert.match(text,/ImageRankerLifetime::RemoveOwnedFiles/);assert.match(text,/saved rankings, cache and recovery data are preserved/);
});
test('native acquisition precedes both initialization effect paths, and every package section validates',()=>{
  const install=text.slice(text.indexOf('Function .onInit'),text.indexOf('Section WebView2'));
  assert.ok(install.indexOf('AcquireInstall')<install.indexOf('!insertmacro SetContext'));
  const uninstall=text.slice(text.indexOf('Function un.onInit'),text.indexOf('Section Uninstall'));
  assert.ok(uninstall.indexOf('AcquireUninstall')<uninstall.indexOf('!insertmacro SetContext'));
  for(const section of ['WebView2','Install','Uninstall'])assert.match(text,new RegExp('Section '+section+'\\n  !insertmacro ImageRankerValidate'));
  assert.equal((text.match(/ImageRankerLifetime::ReleaseAfterEffects/g)||[]).length,2);
  const init=text.slice(text.indexOf('Function .onInit'),text.indexOf('Section Install'));
  assert.equal(init.includes('BeginInstall'),false);
  const section=text.slice(text.indexOf('Section Install'));assert.ok(section.indexOf('BeginInstall')<section.indexOf('File '));
});
test('no later finish callback launches app or mutates shortcuts after explicit release',()=>{
  for(const forbidden of ['RunMainBinary','MUI_FINISHPAGE_RUN','MUI_FINISHPAGE_SHOWREADME','Function .onInstSuccess','RunAsUser'])assert.equal(text.includes(forbidden),false);
  assert.match(text,/Resources require a new qualified native package layout/);
  assert.match(text,/Only current-user package ownership is supported/);
});
