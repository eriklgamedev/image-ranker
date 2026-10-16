import fs from 'node:fs';
import path from 'node:path';
import {build} from 'esbuild';
import {createFixtures,digest} from '../desktop/fixtures.mjs';

const root=createFixtures();
const inventory=JSON.parse(fs.readFileSync(path.join(root,'inventory.json'),'utf8'));
const template=fs.readFileSync(inventory[0].path);
for(let index=21;index<=148;index++){
  const bytes=Buffer.from(template);
  // Distinct native image bytes and paths, with the original valid BMP header.
  bytes[54]=index;bytes[55]=(index*19)%256;
  const file=path.join(root,'source',`qualification-${String(index).padStart(3,'0')}.bmp`);
  fs.writeFileSync(file,bytes,{flag:'wx'});
  inventory.push({path:file,hash:digest(file)});
}
fs.writeFileSync(path.join(root,'inventory.json'),JSON.stringify(inventory,null,2));
fs.writeFileSync(path.join(root,'source-choice.txt'),'source',{flag:'wx'});
await build({entryPoints:['src/domain/ranking/index.ts'],bundle:true,platform:'node',format:'esm',outfile:path.join(root,'qualification-domain.mjs')});
fs.writeFileSync(path.join(root,'decision-workload.json'),JSON.stringify({
  candidates:143,decisions:10000,minimumMeasuredSamples:200,p95DurableCommandBudgetMs:100,restoreReadyBudgetMs:2000,
  repetitions:1,fixtureClass:'generated 96x64 BMP; no 24 MP image/memory qualification',
  boundary:'Actual domain decide; public save_session invocation through WebView IPC to its durable return; monotonic renderer clock. Verification after the timed return.',
  limitations:['Debug/e2e artifact; not installed release','No physical input or presented-frame measurement','One exploratory integrated run; final repeat matrix still required']},null,2),{flag:'wx'});
console.log(root);
