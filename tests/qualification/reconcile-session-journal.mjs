// Read-only physical-byte audit. This never opens a native SessionStore, repairs
// evidence, creates an ACK, or qualifies the 10,000-decision workload.
// Usage: node tests/qualification/reconcile-session-journal.mjs <fixture-root>
// Redirect stdout to a NEW result file outside the inspected fixture.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {fileURLToPath} from 'node:url';

const sha = value => crypto.createHash('sha256').update(value).digest('hex');
const assert = (condition, message) => { if (!condition) throw new Error(message); };
// Emit sorted entries directly: JavaScript objects otherwise reorder integer
// keys, and UTF-16 sort differs from Rust's UTF-8 ordering for some Unicode keys.
const canonicalJson = value => Array.isArray(value) ? `[${value.map(canonicalJson).join(',')}]`
  : value && typeof value === 'object' ? `{${Object.keys(value).sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b))).map(key => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(',')}}` : JSON.stringify(value);
const digestValue = value => sha(canonicalJson(value));
const clone = value => structuredClone(value);

function dimensions(value) {
  let nodes = 0, depth = 0;
  const stack = [[value, 0]];
  while (stack.length) {
    const [item, level] = stack.pop(); nodes++; depth = Math.max(depth, level);
    assert(typeof item !== 'number' || Number.isSafeInteger(item), 'Offline JS audit cannot exactly certify non-safe-integer native numbers');
    if (item && typeof item === 'object') for (const child of Object.values(item)) stack.push([child, level + 1]);
  }
  return {nodes, depth};
}

// Same legal path semantics as state_log::apply, including ordered array edits.
function apply(value, changes) {
  for (const change of changes) {
    assert(Array.isArray(change.path) && typeof change.remove === 'boolean', 'Invalid delta shape');
    if (!change.path.length) { assert(!change.remove, 'Cannot remove root'); value = clone(change.value); continue; }
    let parent = value;
    for (const part of change.path.slice(0, -1)) {
      assert(parent && typeof parent === 'object' && Object.hasOwn(parent, part), 'Invalid delta parent');
      parent = parent[part];
    }
    const last = change.path.at(-1);
    assert(parent && typeof parent === 'object', 'Invalid delta collection');
    if (Array.isArray(parent)) {
      assert(/^(0|[1-9][0-9]*)$/.test(last), 'Invalid delta index');
      const index = Number(last);
      assert(index <= parent.length && (!change.remove || index < parent.length), 'Delta index gap');
      if (change.remove) parent.splice(index, 1); else parent[index] = clone(change.value);
    } else {
      assert(!['__proto__', 'constructor', 'prototype'].includes(last), 'Unsafe JS audit key');
      if (change.remove) { assert(Object.hasOwn(parent, last), 'Invalid removal'); delete parent[last]; }
      else Object.defineProperty(parent, last, {value: clone(change.value), configurable: true, enumerable: true, writable: true});
    }
  }
  return value;
}

export function reconcile(fixtureRoot) {
  const root = path.resolve(fixtureRoot), directory = path.join(root, 'app-data', 'sessions');
  const inputs = [];
  const read = name => {
    const filename = path.join(directory, name), bytes = fs.readFileSync(filename);
    inputs.push({path: path.relative(root, filename), sha256: sha(bytes), bytes: bytes.length});
    return bytes;
  };
  const envelope = JSON.parse(read('session-journal.authority.json'));
  dimensions(envelope);
  const authority = envelope.authority;
  assert(envelope.checksum === digestValue(authority), 'Authority checksum mismatch');
  assert(authority.schemaVersion === 1 && /^[0-9a-f-]{36}$/i.test(authority.physicalGeneration), 'Unsupported authority');
  const logName = `session-events-${authority.physicalGeneration}.log`, bytes = read(logName);
  assert(bytes.length <= 256 * 1024 * 1024, 'Log exceeds native read budget');
  const events = [], ids = new Set();
  const prefixes = [sha(`ImageRanker/v2:${authority.sessionId}:${authority.logGeneration}`)];
  let offset = 0, nodes = 0, depth = 0, largestFrameBytes = 0, tornTail = false;
  while (offset < bytes.length) {
    if (bytes.length - offset < 40) { tornTail = true; break; }
    assert(bytes.toString('ascii', offset, offset + 4) === 'IRL1', `Bad frame magic at ${offset}`);
    const length = bytes.readUInt32LE(offset + 4);
    assert(length <= 16 * 1024 * 1024, `Oversized frame at ${offset}`);
    if (bytes.length - offset - 40 < length) { tornTail = true; break; }
    const payload = bytes.subarray(offset + 40, offset + 40 + length), hash = sha(payload);
    assert(hash === bytes.subarray(offset + 8, offset + 40).toString('hex'), `Checksum mismatch at ${offset}`);
    const event = JSON.parse(payload), usage = dimensions(event);
    assert(event.schemaVersion === 2 && event.sequence === events.length + 1, 'Invalid event sequence/version');
    assert(event.sessionId === authority.sessionId && event.logGeneration === authority.logGeneration, 'Event lineage mismatch');
    assert(event.eventId && !ids.has(event.eventId) && event.payload.id === event.eventId, 'Invalid/duplicate event identity');
    ids.add(event.eventId); nodes += usage.nodes; depth = Math.max(depth, usage.depth);
    assert(nodes <= 4_000_000 && depth <= 96 && events.length < 250_000, 'Log structure exceeds native budget');
    prefixes.push(sha(Buffer.concat([Buffer.from(prefixes.at(-1)), Buffer.from(hash, 'hex')])));
    events.push(event); largestFrameBytes = Math.max(largestFrameBytes, length); offset += 40 + length;
  }
  const checkpoints = ['session.json', 'session.backup.json'].map(name => {
    const data = read(name), snapshot = JSON.parse(data);
    dimensions(snapshot);
    const {checksum, ...body} = snapshot;
    assert(checksum === digestValue(body), `${name}: checksum mismatch`);
    assert(body.schemaVersion === 2 && body.sessionId === authority.sessionId && body.logGeneration === authority.logGeneration, `${name}: lineage mismatch`);
    assert(body.prefixDigest === prefixes[body.revision], `${name}: prefix mismatch`);
    return {name, revision: body.revision, bytes: data.length, payload: body.payload, checksumValid: true, prefixValid: true};
  });
  const best = checkpoints.reduce((a, b) => a.revision > b.revision ? a : b);
  let state = clone(best.payload);
  for (const event of events.slice(best.revision)) {
    assert(event.payload.kind === 'state-delta', 'Unsupported replay event kind');
    state = apply(state, event.payload.changes);
  }
  let interruptedAttempt = null;
  const interruptedPath = path.join(root, 'decision-interrupted.json');
  if (fs.existsSync(interruptedPath)) {
    const evidence = fs.readFileSync(interruptedPath), interrupted = JSON.parse(evidence);
    inputs.push({path: path.relative(root, interruptedPath), sha256: sha(evidence), bytes: evidence.length});
    const attempt = interrupted.activeAttempt;
    if (attempt) {
      const expectedRevision = attempt.expectedRevision;
      assert(Number.isSafeInteger(expectedRevision) && expectedRevision > 0, 'Invalid attempted revision');
      interruptedAttempt = {
        index: attempt.index,
        expectedRevision,
        expectedRevisionFramePresent: events.some(event => event.sequence === expectedRevision),
        acknowledgedDecisions: interrupted.acknowledged,
        lastVerifiedDurableRevision: interrupted.lastVerifiedDurableRevision,
      };
    }
  }
  // Re-read exactly the inspected files; detect audit-time mutation, not OS ownership.
  for (const input of inputs) assert(sha(fs.readFileSync(path.join(root, input.path))) === input.sha256, `Input changed: ${input.path}`);
  const history = state.ui?.run?.history;
  const length = Array.isArray(history) ? history.length : history?.length;
  return {
    kind: 'offline-session-byte-reconciliation', fixtureRoot: root,
    limitations: ['Read-only physical byte audit; not native ownership validation or an ACK.', 'Does not qualify restart/recovery UI, complete 10,000 decisions, Undo, or a released application.'],
    utilitySha256: sha(fs.readFileSync(fileURLToPath(import.meta.url))), inputs,
    journal: {frames: events.length, bytes: bytes.length, validBytes: offset, tornTail, nodes, depth, largestFrameBytes, finalPrefix: prefixes.at(-1)},
    checkpoints: checkpoints.map(({payload, ...item}) => item),
    replay: {nativeRevision: events.at(-1)?.sequence ?? 0, historyLength: length, uiRevision: state.ui?.run?.revision, uiBytes: Buffer.byteLength(JSON.stringify(state.ui)), wrapperBytes: Buffer.byteLength(JSON.stringify(state)), lastCommitAt: events.at(-1)?.committedAt},
    interruptedAttempt,
    inputsUnchanged: true,
  };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  assert(process.argv.length === 3, 'Expected exactly one fixture root');
  process.stdout.write(JSON.stringify(reconcile(process.argv[2]), null, 2) + '\n');
}
