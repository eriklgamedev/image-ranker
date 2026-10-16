// Evidence operations only. This module never builds, downloads, installs, or launches the app.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {fileURLToPath} from 'node:url';

export const target = 'x86_64-pc-windows-msvc';
export function sha(bytes) { return crypto.createHash('sha256').update(bytes).digest('hex'); }
export function hashFile(file) { return sha(fs.readFileSync(file)); }
function under(root, file) {
  const relative=path.relative(root,file);
  if (!relative || relative.startsWith(`..${path.sep}`) || relative==='..' || path.isAbsolute(relative)) throw new Error(`Path is outside its owned directory: ${file}`);
  return relative.replaceAll('\\','/');
}
export function walk(directory) {
  if (!fs.existsSync(directory)) return [];
  if (fs.lstatSync(directory).isSymbolicLink()) throw new Error(`Linked input directory is not qualified: ${directory}`);
  return fs.readdirSync(directory,{withFileTypes:true}).flatMap(entry=>{
    const file=path.join(directory,entry.name);
    if(entry.isSymbolicLink())throw new Error(`Linked input is not qualified: ${file}`);
    return entry.isDirectory()?walk(file):entry.isFile()?[file]:[];
  }).sort();
}
export function installerInputs(root){
  const trees=['src-tauri/install-lifetime-core/src','installer/nsis','installer/nsis-plugin/src','installer/nsis-plugin/upstream','installer/nsis-plugin/artifacts'];
  const files=trees.flatMap(tree=>walk(path.join(root,tree)));
  for(const crate of ['src-tauri/install-lifetime-core','installer/nsis-plugin'])for(const name of ['Cargo.toml','Cargo.lock','build.rs','.cargo/config.toml']){
    const file=path.join(root,crate,name);if(fs.existsSync(file))files.push(file);
  }
  return files;
}
export function captureFiles(root, files) {
  return [...new Set(files)].sort().map(file=>{
    const relative=under(root,file);
    const stat=fs.lstatSync(file);
    if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`Expected ordinary input file: ${file}`);
    return {path:relative,bytes:stat.size,sha256:hashFile(file)};
  });
}
export function inputManifest(root) {
  root=path.resolve(root);
  const trees=['src','src-tauri/src','src-tauri/capabilities','src-tauri/icons','assets','scripts','.cargo',
    'node_modules/@tauri-apps/cli','node_modules/@tauri-apps/cli-win32-x64-msvc'];
  const files=[...trees.flatMap(tree=>walk(path.join(root,tree))),...installerInputs(root)];
  const required=['index.html','package.json','package-lock.json','vite.config.ts','tsconfig.json','rust-toolchain.toml',
    'src-tauri/build.rs','src-tauri/Cargo.toml','src-tauri/Cargo.lock','src-tauri/tauri.conf.json',
    'src-tauri/tauri.windows-qualification.conf.json','tests/qualification/package-capture.mjs',
    'tests/qualification/windows-package-capture.ps1','tests/qualification/validate-config.mjs','tests/qualification/schema.mjs'];
  for(const relative of required) { const file=path.join(root,relative); if(!fs.existsSync(file))throw new Error(`Missing build input: ${relative}`);files.push(file); }
  for(const folder of ['', 'src-tauri']) {
    for(const name of fs.readdirSync(path.join(root,folder))) {
      if(/^(?:tauri.*\.json5?|Tauri.*\.toml|tsconfig.*\.json|vite\.config\.[a-z]+|\.env(?:\..*)?|\.npmrc|\.nvmrc|\.node-version)$/.test(name)) {
        // Environment files may contain secrets and alter production builds. Do not copy them into evidence.
        if(name.startsWith('.env'))throw new Error(`Unreviewed environment build input ${folder}/${name}; remove it from this qualification workspace first`);
        files.push(path.join(root,folder,name));
      }
    }
  }
  for(const relative of ['.cargo-home/config','.cargo-home/config.toml','node_modules/.package-lock.json']) if(fs.existsSync(path.join(root,relative)))files.push(path.join(root,relative));
  const inputs=captureFiles(root,files);
  // Cargo also observes ancestor .cargo configurations, even with CARGO_HOME set.
  // Preserve their exact hashes without copying possible credentials into logs.
  let ancestor=path.dirname(root);
  while(ancestor!==root) {
    for(const name of ['config','config.toml']) {
      const file=path.join(ancestor,'.cargo',name);
      if(fs.existsSync(file)) {
        const stat=fs.lstatSync(file);
        if(!stat.isFile() || stat.isSymbolicLink())throw new Error(`Unqualified ancestor Cargo config: ${file}`);
        inputs.push({path:`external-cargo-config:${file}`,bytes:stat.size,sha256:hashFile(file)});
      }
    }
    const parent=path.dirname(ancestor);if(parent===ancestor)break;ancestor=parent;
  }
  inputs.sort((a,b)=>a.path.localeCompare(b.path));
  return {schemaVersion:1,createdAt:new Date().toISOString(),root,inputs,treeSha256:sha(JSON.stringify(inputs)),distExcluded:'Generated frontend output is captured after beforeBuild; it is not an immutable input.'};
}
export function compareManifests(before, after) {
  const old=new Map(before.inputs.map(value=>[value.path,value]));
  const next=new Map(after.inputs.map(value=>[value.path,value]));
  const changes=[];
  for(const [name,value] of old) {
    const present=next.get(name);
    if(!present)changes.push({path:name,kind:'removed'});
    else if(present.sha256!==value.sha256 || present.bytes!==value.bytes)changes.push({path:name,kind:'changed'});
  }
  for(const name of next.keys())if(!old.has(name))changes.push({path:name,kind:'added'});
  return {unchanged:changes.length===0,beforeTreeSha256:before.treeSha256,afterTreeSha256:after.treeSha256,changes};
}
export function assertProductionFeatures(records) {
  const roots=records.filter(record=>record.path.includes('/.fingerprint/image-ranker-') && /\/(?:lib-image_ranker_lib|bin-image-ranker)\.json$/.test(record.path));
  if (!roots.some(record=>record.path.endsWith('/bin-image-ranker.json')) || !roots.some(record=>record.path.endsWith('/lib-image_ranker_lib.json'))) throw new Error('No complete release application feature evidence was produced');
  for(const record of roots) {
    const features=JSON.parse(record.value.features);
    if(!Array.isArray(features) || features.some(feature=>typeof feature!=='string'))throw new Error('Unknown Cargo feature fingerprint schema');
    if(features.includes('e2e') || features.includes('default'))throw new Error('Production application enabled e2e or default features');
  }
  for(const record of records)if(/\/\.fingerprint\/tauri-plugin-wdio(?:-webdriver)?-/.test(record.path))throw new Error('Automation dependency was compiled into this target directory');
  return {rootFeatures:roots.map(record=>({path:record.path,features:JSON.parse(record.value.features)})),automationDependenciesFound:false};
}
export function collectOutputs(root, targetRoot, evidence) {
  const release=path.join(targetRoot,target,'release');
  const app=path.join(release,'image-ranker.exe');
  const installers=walk(path.join(release,'bundle','nsis')).filter(file=>file.toLowerCase().endsWith('.exe'));
  const candidates=[...(fs.existsSync(app)?[app]:[]),...installers];
  const artifacts=candidates.map(file=>({kind:file===app?'application':'nsis-installer',path:file,relativePath:under(targetRoot,file),bytes:fs.statSync(file).size,sha256:hashFile(file)}));
  const fingerprintFiles=[...walk(path.join(targetRoot,'release','.fingerprint')),...walk(path.join(release,'.fingerprint'))].filter(file=>file.endsWith('.json'));
  const fingerprints=fingerprintFiles.map(file=>({path:under(targetRoot,file),sha256:hashFile(file),raw:fs.readFileSync(file,'utf8')}));
  writeNew(path.join(evidence,'cargo-fingerprints.json'),{schemaVersion:1,records:fingerprints});
  let featureEvidence=null,featureError=null;
  try { featureEvidence=assertProductionFeatures(fingerprints.map(record=>({...record,value:JSON.parse(record.raw)}))); } catch(error) { featureError=String(error); }
  const dist=captureFiles(root,walk(path.join(root,'dist')));
  const installerEvidence=collectInstallerEvidence(root,targetRoot,evidence);
  return {artifacts,generatedFrontend:dist,featureEvidence,featureError,installerEvidence,installed:false,signedReleaseQualified:false};
}
// FIX 1: The qualification overlay uses Tauri's local tools directory. Capture
// generated includes and the complete fresh tool cache, not an assumed user cache.
export function collectInstallerEvidence(root,targetRoot,evidence){
  const generated=walk(path.join(targetRoot,target,'release','nsis')).filter(file=>/\.(?:nsi|nsh)$/i.test(file));
  const rendered=captureFiles(targetRoot,generated);
  const tools=captureFiles(targetRoot,walk(path.join(targetRoot,'.tauri')));
  const pluginPath=path.join(root,'installer/nsis-plugin/artifacts/ImageRankerLifetime.dll');
  let plugin=null,pluginError=null;
  if(fs.existsSync(pluginPath)){
    const captured=captureFiles(root,[pluginPath])[0];
    try{plugin={...captured,absolutePath:pluginPath,machine:peMachine(pluginPath)};if(plugin.machine!==0x14c)throw Error('NSIS plugin is not i686');}
    catch(error){pluginError=String(error);}
  }else pluginError='No staged in-process NSIS plugin was produced';
  // FIX 2: Retain exact rendered bytes separately for review; never overwrite
  // prior evidence. These files can contain local paths and stay local.
  let total=0;
  for(const item of rendered){
    total+=item.bytes;if(item.bytes>16*1024*1024||total>32*1024*1024)throw Error('Rendered installer inputs exceed evidence bound');
    const to=path.join(evidence,'rendered-installer',item.path);fs.mkdirSync(path.dirname(to),{recursive:true});
    fs.copyFileSync(path.join(targetRoot,item.path),to,fs.constants.COPYFILE_EXCL);
    if(hashFile(to)!==item.sha256)throw Error('Rendered installer input changed during capture');
  }
  const issues=[];
  if(!rendered.some(item=>item.path.endsWith('/installer.nsi')))issues.push('No rendered installer script was produced');
  if(!tools.some(item=>item.path.toLowerCase()==='.tauri/nsis/makensis.exe'))issues.push('No local NSIS compiler/toolset was produced');
  if(pluginError)issues.push(pluginError);
  return{rendered,tools,plugin,issues,scope:'Post-build observation of exact local tools/rendered inputs and staged plugin; no installation or lifetime qualification.'};
}
// FIX 3: Read the actual PE machine field; a filename or build label cannot
// qualify the Unicode x86 plugin ABI. Hash capture still covers the entire file.
export function peMachine(file){
  const stat=fs.lstatSync(file);if(!stat.isFile()||stat.isSymbolicLink()||stat.size<64)throw Error('Invalid PE input');
  const descriptor=fs.openSync(file,'r');
  try{
    const header=Buffer.alloc(64);if(fs.readSync(descriptor,header,0,64,0)!==64||header.readUInt16LE(0)!==0x5a4d)throw Error('Invalid DOS header');
    const offset=header.readUInt32LE(60);if(offset<64||offset>stat.size-6)throw Error('PE header is outside the file');
    const pe=Buffer.alloc(6);if(fs.readSync(descriptor,pe,0,6,offset)!==6||pe.readUInt32LE(0)!==0x4550)throw Error('Invalid PE signature');
    return pe.readUInt16LE(4);
  }finally{fs.closeSync(descriptor);}
}
export function writeNew(file,value) {fs.writeFileSync(file,JSON.stringify(value,null,2)+'\n',{flag:'wx'});}
export function classifyFailure(text) {
  const categories=[];
  if(/no matching package|failed to download|attempting to make an HTTP request.*offline|could not find.*registry|failed to get.*dependency/is.test(text))categories.push('dependency-unavailable');
  if(/webview2|NSIS|makensis/i.test(text))categories.push('windows-bundler-or-runtime');
  if(/download|network|dns|http request|proxy|certificate|connection|os error 100/i.test(text))categories.push('download-or-network');
  return {categories:categories.length?categories:['unclassified-command-failure'],classification:'Diagnostic hints only; exact stdout/stderr and exit status are authoritative.'};
}
if(process.argv[1] && path.resolve(process.argv[1])===fileURLToPath(import.meta.url)) {
  try {
    const [action,root,evidence,targetRoot]=process.argv.slice(2);
    if(action==='before')writeNew(path.join(evidence,'inputs-before.json'),inputManifest(root));
    else if(action==='after'){
      const before=JSON.parse(fs.readFileSync(path.join(evidence,'inputs-before.json'),'utf8'));
      const after=inputManifest(root);writeNew(path.join(evidence,'inputs-after.json'),after);
      const verification=compareManifests(before,after);writeNew(path.join(evidence,'input-verification.json'),verification);
      if(!verification.unchanged)process.exitCode=2;
    } else if(action==='outputs')writeNew(path.join(evidence,'outputs.json'),collectOutputs(path.resolve(root),path.resolve(targetRoot),evidence));
    else throw new Error('Expected before, after, or outputs action');
  } catch(error){console.error(String(error));process.exitCode=1;}
}
