import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {validateSchema, mergePatch} from './schema.mjs';

export function validatePackageConfig(root) {
  const sources = [];
  const read = relative => {
    const bytes = fs.readFileSync(path.join(root, relative));
    sources.push({path: relative, sha256: createHash('sha256').update(bytes).digest('hex')});
    return JSON.parse(bytes.toString('utf8').replace(/^\uFEFF/, ''));
  };
  for (const file of ['tauri.conf.json5', 'Tauri.toml', 'tauri.windows.conf.json5', 'Tauri.windows.toml']) {
    if (fs.existsSync(path.join(root, 'src-tauri', file))) throw new Error(`Unmodelled configuration ${file}; review actual merge first`);
  }
  const schema = read('node_modules/@tauri-apps/cli/config.schema.json');
  const base = read('src-tauri/tauri.conf.json');
  const platform = fs.existsSync(path.join(root, 'src-tauri/tauri.windows.conf.json')) ? read('src-tauri/tauri.windows.conf.json') : {};
  const overlay = read('src-tauri/tauri.windows-qualification.conf.json');
  const config = mergePatch(mergePatch(base, platform), overlay);
  const errors = validateSchema(config, schema);
  if (errors.length) throw new Error(errors.join('\n'));
  if (config.identifier !== base.identifier || config.productName !== base.productName || config.version !== base.version) throw new Error('Qualification changed app identity/version');
  if (JSON.stringify(config.build.features) !== '[]' || config.app.withGlobalTauri === true) throw new Error('Unexpected production feature/global bridge setting');
  const windows = config.bundle.windows;
  if (!config.bundle.active || JSON.stringify(config.bundle.targets) !== '["nsis"]' ||
      windows.allowDowngrades !== false || windows.nsis.installMode !== 'currentUser' ||
      windows.webviewInstallMode.type !== 'offlineInstaller' || windows.webviewInstallMode.silent !== true) throw new Error('Windows package contract mismatch');
  return {validation: 'passed', schemaId: schema.$id, network: 'not-used',
    scope: 'merged base + optional Windows JSON + qualification overlay; supported encountered draft-07 keywords',
    sources, merged: config, qualification: 'not-built-or-installed'};
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { console.log(JSON.stringify(validatePackageConfig(path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')), null, 2)); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
