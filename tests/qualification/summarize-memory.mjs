import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {summarizeMemory} from './process-model.mjs';

export function readMemoryCapture(directory) {
  const log = path.join(directory,'memory.jsonl');
  if (fs.statSync(log).size > 64*1024*1024) throw new Error('Capture exceeds bounded parser input');
  const content = fs.readFileSync(log,'utf8');
  if (!content.endsWith('\n')) throw new Error('Incomplete final sample: preserve capture; do not silently trim');
  const lines = content.slice(0,-1).split('\n');
  if (lines.length > 36001) throw new Error('Too many samples');
  const records = lines.map(line=>{
    if (Buffer.byteLength(line)>1024*1024) throw new Error('Oversize sample');
    return JSON.parse(line);
  });
  const terminalPath = path.join(directory,'sampler-summary.json');
  if (fs.existsSync(terminalPath) && fs.statSync(terminalPath).size > 1024*1024) throw new Error('Oversize terminal record');
  const terminal = fs.existsSync(terminalPath) ? JSON.parse(fs.readFileSync(terminalPath,'utf8')) : null;
  return summarizeMemory(records,terminal);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv.length!==3) throw new Error('Usage: node tests/qualification/summarize-memory.mjs CAPTURE_DIRECTORY');
    console.log(JSON.stringify(readMemoryCapture(path.resolve(process.argv[2])),null,2));
  } catch (error) {console.error(error.message);process.exitCode=1;}
}
