import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';

export function digest(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

function bitmap(index) {
  const width = 96, height = 64, stride = width * 3;
  const bytes = Buffer.alloc(54 + stride * height);
  bytes.write('BM');
  bytes.writeUInt32LE(bytes.length, 2);
  bytes.writeUInt32LE(54, 10);
  bytes.writeUInt32LE(40, 14);
  bytes.writeInt32LE(width, 18);
  bytes.writeInt32LE(height, 22);
  bytes.writeUInt16LE(1, 26);
  bytes.writeUInt16LE(24, 28);
  bytes.writeUInt32LE(stride * height, 34);
  for (let y = 0; y < height; y++) for (let x = 0; x < width; x++) {
    const offset = 54 + y * stride + x * 3;
    bytes[offset] = (index * 23 + x * 2) % 256;
    bytes[offset + 1] = (index * 41 + y * 3) % 256;
    bytes[offset + 2] = (index * 67 + x + y) % 256;
  }
  return bytes;
}

export function createFixtures() {
  const parent = path.resolve('tests-artifacts');
  fs.mkdirSync(parent, {recursive: true});
  const root = fs.mkdtempSync(path.join(parent, 'desktop-'));
  fs.writeFileSync(path.join(root, '.image-ranker-e2e'), 'image-ranker-disposable-fixture-v1\n');
  fs.mkdirSync(path.join(root, 'source', 'nested'), {recursive: true});
  fs.mkdirSync(path.join(root, 'output'));
  for (const directory of ['recovery-profile', 'another-profile', 'occupied']) {
    fs.mkdirSync(path.join(root, directory));
  }
  fs.writeFileSync(path.join(root, 'occupied', 'preserve.txt'), 'This occupied folder must never be initialized as a recovery profile.');
  const originals = [];
  for (let i = 1; i <= 20; i++) {
    const file = path.join(root, i > 15 ? 'source/nested' : 'source', `fixture-${String(i).padStart(2, '0')}.bmp`);
    fs.writeFileSync(file, bitmap(i));
    originals.push({path: file, hash: digest(file)});
  }
  fs.writeFileSync(path.join(root, 'source', 'corrupt.bmp'), 'not a bitmap');
  fs.writeFileSync(path.join(root, 'source', 'notes.txt'), 'An unsupported file must remain unchanged.');
  fs.writeFileSync(path.join(root, 'inventory.json'), JSON.stringify(originals, null, 2));
  return root;
}
