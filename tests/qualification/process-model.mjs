// Pure interpretation of native sampler records. No OS calls or process control.
export function processKey(pid, created) {
  if (!Number.isInteger(pid) || pid <= 0 || typeof created !== 'string' || !/^[0-9]{1,20}$/.test(created)) throw new Error('Invalid process identity');
  return `${pid}:${BigInt(created)}`;
}

export function mayAdmitChild(parent, child) {
  if (!parent || !child || parent.status !== 'sampled' || child.parentPid !== parent.pid || child.parentCreated !== parent.created) return false;
  return BigInt(child.created) >= BigInt(parent.created) && child.pid !== parent.pid;
}

function bytes(value) {
  if (!Number.isSafeInteger(value) || value < 0) throw new Error('Invalid or unsafe byte counter');
  return value;
}

export function summarizeMemory(records, samplerResult) {
  if (!Array.isArray(records) || !records.length || records[0].type !== 'header' || records[0].schema !== 1) throw new Error('Missing sampler header');
  const header = records[0];
  if (header.coverage !== 'attach-polling-only') throw new Error('Unsupported coverage producer');
  const rootKey = processKey(header.rootPid, header.rootCreated);
  const known = new Map([[rootKey, {pid:header.rootPid, created:header.rootCreated, status:'sampled'}]]);
  const seen = new Set([rootKey]), exited = new Set();
  const issues = new Set(['polling-can-miss-short-lived-processes', 'attach-does-not-cover-launch', 'gpu-not-measured']);
  let samples = 0, completeObservedSamples = 0, workingPeak = null, privatePeak = null;
  let previousStart = null, previousEnd = null, maxGapMs = 0, maxCollectionSpanMs = 0;
  const perProcessPeaks = new Map();
  for (const record of records.slice(1)) {
    if (record.type !== 'sample' || record.sequence !== samples || !Number.isFinite(record.startMs) ||
        !Number.isFinite(record.endMs) || record.startMs < 0 || record.endMs < record.startMs ||
        (previousStart !== null && record.startMs < previousEnd)) throw new Error('Invalid sample order/time');
    if (!Array.isArray(record.processes) || record.processes.length > 512 || !Array.isArray(record.issues)) throw new Error('Invalid sample shape');
    if (previousStart !== null) maxGapMs = Math.max(maxGapMs, record.startMs - previousStart);
    maxCollectionSpanMs = Math.max(maxCollectionSpanMs, record.endMs-record.startMs);
    previousStart = record.startMs;
    previousEnd = record.endMs;
    const incoming = new Map();
    for (const row of record.processes) {
      const key = processKey(row.pid, row.created);
      if (incoming.has(key)) throw new Error('Duplicate process identity in sample');
      if (!['sampled','query-failed','exited'].includes(row.status)) throw new Error('Unknown process status');
      incoming.set(key, row);
    }
    // Admit only exact children of a retained parent identity. Exited parents
    // cannot prove a newly observed child's lineage; already admitted children
    // remain members through parent exit. Snapshot order is not significant.
    let progress = true;
    const unresolved = new Map([...incoming].filter(([key]) => !seen.has(key)));
    while (progress) {
      progress = false;
      for (const [key,row] of unresolved) {
        const parentKey = processKey(row.parentPid, row.parentCreated);
        const parent = incoming.get(parentKey) ?? known.get(parentKey);
        if (known.has(parentKey) && mayAdmitChild(parent,row)) { known.set(key,row);seen.add(key); unresolved.delete(key); progress=true; }
      }
    }
    if (unresolved.size) throw new Error('Unproved process membership; refusing unrelated/reused-PID counters');
    for (const [key,row] of incoming) if (exited.has(key) && row.status!=='exited') throw new Error('Exited process identity reappeared');
    let working = 0, privateCommit = 0, complete = incoming.size > 0;
    for (const [key, previous] of known) {
      const row = incoming.get(key);
      if (!row) {
        if (previous.status !== 'exited') { complete=false;issues.add('known-live-process-missing'); }
        continue;
      }
      if (row.status === 'exited') {known.delete(key);exited.add(key);}
      else known.set(key,row);
      if (row.status === 'query-failed') { complete=false;issues.add('member-query-failed'); }
      if (row.status === 'sampled') {
        working += bytes(row.workingSetBytes);
        privateCommit += bytes(row.privateCommitBytes);
        perProcessPeaks.set(key,Math.max(perProcessPeaks.get(key)??0,bytes(row.lifetimePeakWorkingSetBytes)));
      }
    }
    if (!Number.isSafeInteger(working) || !Number.isSafeInteger(privateCommit)) throw new Error('Unsafe aggregate');
    for (const issue of record.issues) {
      if (typeof issue !== 'string' || issue.length > 200) throw new Error('Invalid issue');
      issues.add(issue); complete=false;
    }
    if (complete) {
      completeObservedSamples++;
      workingPeak = Math.max(workingPeak??0,working);
      privatePeak = Math.max(privatePeak??0,privateCommit);
    }
    samples++;
  }
  if (!samplerResult || samplerResult.schema !== 1 || samplerResult.samples !== samples || samplerResult.coverage !== header.coverage) issues.add('missing-or-inconsistent-terminal-record');
  for (const issue of samplerResult?.issues ?? []) issues.add(issue);
  if (samplerResult?.fatal) issues.add('sampler-error');
  if (samplerResult?.survivingObservedProcesses?.length) issues.add('observed-processes-still-live');
  if (maxGapMs > header.intervalMs*2) issues.add('sampling-gap-over-two-intervals');
  if (samplerResult?.stopReason !== 'observed-members-exited') issues.add('bounded-capture-not-full-lifetime');
  return {
    schema:1, samples, completeObservedSamples, processIdentities:seen.size,
    coverage:'attach-polling-only', peakQualification:'not-established', gpuMeasurement:'unavailable',
    sampledSumWorkingSetPeakBytes:workingPeak, sampledSumPrivateCommitPeakBytes:privatePeak,
    maxGapMs,maxCollectionSpanMs,issues:[...issues].sort(),
    perProcessLifetimeWorkingSetPeaks:[...perProcessPeaks].map(([identity,value])=>({identity,bytes:value})),
    labels:{workingSet:'sampled process sum; shared pages may be counted more than once; not unique RAM',
      privateCommit:'sampled private committed bytes; not heap, resident set or GPU',
      lifetimePeaks:'per-process diagnostics only; never summed into a simultaneous tree peak',
      sampling:'sequential queries within the recorded collection span; unsampled peaks may be higher'},
  };
}
