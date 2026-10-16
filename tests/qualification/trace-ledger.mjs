// Pure evidence interpretation. These functions neither control ETW nor grant
// authority to operate on a process. Native capture qualification is separate.
const U64 = (1n << 64n) - 1n;
const SECOND = 10_000_000n;
const MAX_EVENTS = 250_000;

function integer(value, label) {
  if (typeof value !== 'string' || !/^(0|[1-9][0-9]{0,19})$/.test(value)) throw new Error(`Invalid ${label}`);
  const result = BigInt(value);
  if (result > U64) throw new Error(`Overflow ${label}`);
  return result;
}
function pid(value) {
  if (!Number.isInteger(value) || value < 1 || value > 0xffffffff) throw new Error('Invalid PID');
  return value;
}
export function traceIdentity(processId, created) {
  return `${pid(processId)}:${integer(created, 'creation FILETIME')}`;
}
function loss(value) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error('Invalid loss counter');
  return value;
}
function issueText(value) {
  if (typeof value !== 'string' || value.length < 1 || value.length > 512) throw new Error('Invalid evidence issue');
  return value;
}
function maximum(previous, incoming) {
  return previous === null || incoming > previous ? incoming : previous;
}

export function summarizeTrace(capture) {
  if (capture?.schema !== 1 || !Array.isArray(capture.events) || capture.events.length > MAX_EVENTS) throw new Error('Invalid bounded trace input');
  const ready = integer(capture.readyFileTime, 'ready FILETIME');
  const rootKey = traceIdentity(capture.root?.pid, capture.root?.created);
  const bound = integer(capture.root.boundFileTime, 'root binding FILETIME');
  const issues = new Set();
  if (capture.root.verified !== true || integer(capture.root.created, 'root creation') < ready || bound < ready) issues.add('root-not-qualified-after-prelaunch-readiness');
  const rows = new Map(), stops = [], counters = [];
  for (const event of capture.events) {
    if (!event || typeof event !== 'object') throw new Error('Invalid event');
    const at = integer(event.at, 'event FILETIME');
    if (event.type === 'issue') { issues.add(issueText(event.reason)); continue; }
    if (event.type === 'start') {
      const key = traceIdentity(event.pid, event.created);
      pid(event.parentPid);
      if (integer(event.created, 'creation') > at) issues.add('process-start-precedes-native-creation');
      const previous = rows.get(key);
      if (previous) {
        if (previous.parentPid !== event.parentPid || previous.at !== at) issues.add('conflicting-start-for-native-identity');
        continue;
      }
      rows.set(key, {key, pid:event.pid, created:event.created, parentPid:event.parentPid, at, exit:null, stopAt:null, peak:null, counterAt:null, parent:null});
      if(rows.size>4096)throw new Error('Process identity capacity exceeded');
    } else if (event.type === 'stop') {
      traceIdentity(event.pid, event.created);
      integer(event.exit, 'exit FILETIME');
      stops.push(event);
    } else if (event.type === 'terminal-counter') {
      pid(event.pid); integer(event.peakWorkingSetBytes, 'terminal peak'); counters.push(event);
    } else throw new Error('Unknown normalized event type');
  }
  for (const event of stops) {
    const row = rows.get(traceIdentity(event.pid, event.created));
    // A process already running when tracing began can stop without a start
    // record. It cannot be a descendant of the post-ready root. Loss counters
    // and the required root/member records still gate qualification.
    if (!row) continue;
    const exit = integer(event.exit, 'exit'), at = integer(event.at, 'stop time');
    if (exit < integer(row.created, 'creation') || at < row.at) { issues.add('invalid-native-process-lifetime'); continue; }
    if (row.exit !== null && (row.exit !== exit || row.stopAt !== at)) issues.add('conflicting-native-stop');
    row.exit = exit; row.stopAt = at;
  }
  const byPid = new Map();
  for (const row of rows.values()) {
    const list = byPid.get(row.pid) ?? []; list.push(row); byPid.set(row.pid,list);
  }
  for (const list of byPid.values()) {
    list.sort((a,b)=>a.at < b.at ? -1 : a.at > b.at ? 1 : 0);
    for (let i=1;i<list.length;i++) if (list[i-1].exit === null || list[i-1].exit >= integer(list[i].created,'creation')) issues.add('overlapping-reused-pid-lifetimes');
  }
  const root = rows.get(rootKey);
  if (!root || root.at < ready || root.at > bound) issues.add('qualified-root-start-missing');
  for (const row of rows.values()) {
    if (row.key === rootKey) continue;
    const created = integer(row.created,'creation');
    const parents = (byPid.get(row.parentPid) ?? []).filter(p=>p.key !== row.key && integer(p.created,'parent creation') <= created && (p.exit === null || p.exit >= created));
    if (parents.length === 1) row.parent = parents[0].key;
  }
  const members = new Set(root ? [rootKey] : []);
  const children=new Map();
  for(const row of rows.values())if(row.parent){const list=children.get(row.parent)??[];list.push(row.key);children.set(row.parent,list);}
  const pending=[...members];
  for(let index=0;index<pending.length;index++){
    for(const child of children.get(pending[index])??[])if(!members.has(child)){members.add(child);pending.push(child);}
  }
  for (const row of rows.values()) {
    // A child mentioning a member PID but lacking an unambiguous lifetime must
    // not vanish silently from the measured tree.
    if (!members.has(row.key) && (byPid.get(row.parentPid) ?? []).some(p=>members.has(p.key)) && !row.parent) issues.add('unresolved-descendant-parent-lifetime');
  }
  if(counters.length>rows.size*2+4096)throw new Error('Terminal counter density exceeds bounded input');
  for (const event of counters) {
    const at = integer(event.at,'counter time');
    const candidates = (byPid.get(event.pid) ?? []).filter(row=>row.stopAt !== null && at >= row.at && at <= row.stopAt + SECOND);
    if (candidates.length !== 1) {
      if ((byPid.get(event.pid) ?? []).some(row=>members.has(row.key))) issues.add('ambiguous-or-unmatched-terminal-counter');
      continue;
    }
    const row = candidates[0], peak = integer(event.peakWorkingSetBytes,'peak');
    if (row.peak !== null && (row.peak !== peak || row.counterAt !== at)) issues.add('conflicting-terminal-counter');
    row.peak=peak; row.counterAt=at;
  }
  const terminal = capture.terminal;
  if (!terminal || terminal.stopConfirmed !== true || terminal.decodeComplete !== true) issues.add('trace-terminal-proof-missing');
  for (const key of ['eventsLost','logBuffersLost','realTimeBuffersLost','consumerLost','decodeErrors']) {
    if (terminal?.[key] === undefined) issues.add(`missing-${key}`);
    else if (loss(terminal[key]) !== 0) issues.add(`nonzero-${key}`);
  }
  if (terminal?.reason !== 'cooperative-stop') issues.add('capture-ended-without-cooperative-workload-boundary');
  for (const reason of terminal?.issues ?? []) issues.add(issueText(reason));
  const memberRows = [...members].map(key=>rows.get(key));
  for (const row of memberRows) {
    if (row.exit === null) issues.add('member-lifetime-not-finished');
    if (row.peak === null) issues.add('member-terminal-peak-missing');
  }
  const gpu = summarizeGpu(capture.gpu ?? [], memberRows, issues);
  return {
    schema:1,
    qualification:issues.size === 0 ? 'model-evidence-complete' : 'incomplete',
    nativeCaptureQualified:false,
    processCount:members.size,
    issues:[...issues].sort(),
    processes:memberRows.map(row=>({identity:row.key,parentIdentity:row.parent,created:row.created,exit:row.exit?.toString()??null,terminalPeakWorkingSetBytes:row.peak?.toString()??null})),
    combinedLifetimeWorkingSetPeakBytes:null,
    gpu,
    limits:['Native producer/schema/cleanup qualification is separate from this pure model.', 'Per-process lifetime maxima are never summed.', 'GPU samples are attributed usage; not a lifetime peak or unique physical graphics memory.'],
  };
}

function summarizeGpu(samples, members, issues) {
  if (!Array.isArray(samples) || samples.length > 3600) throw new Error('Invalid bounded GPU input');
  const rows = new Map(members.map(row=>[row.key,row]));
  const peaks = new Map(); let previous = null, accepted = 0, rejected = 0;
  for (const sample of samples) {
    const begin = integer(sample.begin,'GPU begin'), end = integer(sample.end,'GPU end');
    if (end < begin || (previous !== null && begin-previous < SECOND)) throw new Error('GPU collection must be nonoverlapping and at most 1 Hz');
    previous = begin;
    if (!Array.isArray(sample.values) || sample.values.length > 4096) throw new Error('Invalid GPU rows');
    if (sample.error) {issueText(sample.error); rejected++;continue;}
    const seen = new Set();
    for (const value of sample.values) {
      const key=traceIdentity(value.pid,value.created), row=rows.get(key);
      if (!['dedicated','shared'].includes(value.counter) || typeof value.adapter !== 'string' || !/^luid_0x[0-9a-f]+_0x[0-9a-f]+_phys_[0-9]+$/i.test(value.adapter)) throw new Error('Invalid GPU counter identity');
      const id=`${key}/${value.adapter}/${value.counter}`;
      if (seen.has(id)) throw new Error('Duplicate GPU identity/counter');seen.add(id);
      const bytes=integer(value.bytes,'GPU bytes');
      if (!row || ![0,1].includes(value.status) || value.heldBefore !== true || value.aliveAfter !== true || integer(row.created,'creation') > begin || (row.exit !== null && row.exit <= end)) {rejected++;continue;}
      peaks.set(id,maximum(peaks.get(id)??null,bytes));accepted++;
    }
    if(sample.unresolved!==undefined){
      if(!Array.isArray(sample.unresolved)||sample.unresolved.length>4096)throw new Error('Invalid unresolved GPU identities');
      for(const entry of sample.unresolved){pid(entry.pid);issueText(entry.reason);rejected++;}
    }
  }
  // GPU absence/rejection does not invent a CPU trace failure. It is reported as
  // independently incomplete graphics evidence.
  return {measurement:accepted ? 'sampled-only' : 'unavailable',samples:samples.length,acceptedValues:accepted,rejectedValues:rejected,lifetimePeakBytes:null,perIdentitySampledPeaks:[...peaks].map(([identity,bytes])=>({identity,bytes:bytes.toString()}))};
}
