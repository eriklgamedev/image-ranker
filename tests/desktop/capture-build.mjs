// Reusable qualification metadata; this script is not part of the application.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import os from 'node:os';
import {execFileSync} from 'node:child_process';

async function hash(file) {
  const digest=crypto.createHash('sha256');
  for await(const chunk of fs.createReadStream(file)) digest.update(chunk);
  return digest.digest('hex');
}
function files(directory) {
  return fs.readdirSync(directory,{withFileTypes:true}).flatMap(entry=>{
    const file=path.join(directory,entry.name);
    return entry.isDirectory()?files(file):entry.isFile()?[file]:[];
  });
}
const binary=path.resolve('src-tauri/target/debug',process.platform==='win32'?'image-ranker.exe':'image-ranker');
const inputs=[...files('src'),...files('src-tauri/src'),...files('src-tauri/capabilities'),...files('src-tauri/icons'),...files('src-tauri/install-lifetime-core/src'),...files('assets'),
  'index.html','package.json','package-lock.json','vite.config.ts','tsconfig.json','src-tauri/build.rs',
  'rust-toolchain.toml','src-tauri/Cargo.toml','src-tauri/Cargo.lock','src-tauri/install-lifetime-core/Cargo.toml','src-tauri/tauri.conf.json','src-tauri/tauri.e2e.conf.json',
  ...['src-tauri/install-lifetime-core/Cargo.lock','src-tauri/install-lifetime-core/build.rs','src-tauri/install-lifetime-core/.cargo/config.toml'].filter(file=>fs.existsSync(file))].sort();
const sources=[];
for(const file of inputs) sources.push({path:file.replaceAll('\\','/'),sha256:await hash(file)});
const record={createdAt:new Date().toISOString(),binary:await hash(binary),binaryPath:binary,
  platform:process.platform,architecture:process.arch,osRelease:os.release(),node:process.version,
  rustc:execFileSync('rustc',['--version'],{encoding:'utf8',windowsHide:true}).trim(),
  cargo:execFileSync('cargo',['--version'],{encoding:'utf8',windowsHide:true}).trim(),
  feature:'e2e',configuration:'debug/no-bundle',sources,
  sourceTreeSha256:crypto.createHash('sha256').update(JSON.stringify(sources)).digest('hex')};
fs.mkdirSync('tests-artifacts',{recursive:true});
fs.writeFileSync('tests-artifacts/e2e-build-current.json',JSON.stringify(record,null,2)+'\n');
console.log(`Captured e2e binary ${record.binary} and ${sources.length} source/configuration hashes.`);
