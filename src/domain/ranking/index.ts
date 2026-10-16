import { advanceAdaptive, startAdaptive, voteAdaptive } from './adaptive';
import { checkWorkload, estimate } from './estimates';
import { advancePairwise, startPairwise, votePairwise } from './pairwise';
import { pairToken } from './presentation';
import { appendDecision, decisions, writeOutcome } from './storage';
import type { Comparison, RankingMode, RankingRun, Vote } from './types';
import { advanceWinner, startWinner, voteWinner } from './winnerStays';

export type { Comparison, Decision, DecisionHistory, RankingMode, RankingResult, RankingRun, Vote } from './types';
export { estimate, MAX_CANDIDATES, MAX_DECISIONS } from './estimates';
export { getResult } from './result';

function seededOrder(ids: string[], seed: number): string[] {
  const result = ids.slice();
  let state = seed >>> 0;
  const random = () => {
    state = (state + 0x6d2b79f5) | 0;
    let value = Math.imul(state ^ (state >>> 15), 1 | state);
    value = (value + Math.imul(value ^ (value >>> 7), 61 | value)) ^ value;
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296;
  };
  for (let i = result.length - 1; i > 0; i--) {
    const j = Math.floor(random() * (i + 1));
    [result[i], result[j]] = [result[j], result[i]];
  }
  return result;
}

/** Supply IDs in an explicit path order; UUID order cannot recover path ordering. */
export function createRun(ids: string[], mode: RankingMode, seed: number, randomize = true): RankingRun {
  if (!Array.isArray(ids) || ids.some(id => typeof id !== 'string' || !id.length) || new Set(ids).size !== ids.length) {
    throw new Error('Candidates must have distinct nonempty string IDs.');
  }
  if (!Number.isSafeInteger(seed)) throw new Error('Ranking seed must be a safe integer.');
  checkWorkload(ids.length, mode);
  const normalizedSeed = seed >>> 0;
  const order = randomize ? seededOrder(ids, normalizedSeed) : ids.slice();
  const state = mode === 'winner-stays' ? startWinner(order) : mode === 'adaptive' ? startAdaptive(order) : startPairwise(order);
  const run: RankingRun = {
    schemaVersion: 1, algorithmVersion: 1, prngVersion: 'mulberry32-v1',
    mode, seed: normalizedSeed, randomize, candidates: ids.slice(), order,
    indices: Object.fromEntries(order.map((id, index) => [id, index])),
    outcomes: {}, state, pair: null, history: { chunks: [], length: 0 }, revision: 0,
  };
  advance(run);
  return run;
}

function advance(run: RankingRun): void {
  const state = run.state;
  run.pair = state.kind === 'winner-stays' ? advanceWinner(run, state) : state.kind === 'adaptive' ? advanceAdaptive(run, state) : advancePairwise(run, state);
}

export function getPair(run: RankingRun): Comparison | null {
  return run.pair ? { ...run.pair } : null;
}

function apply(run: RankingRun, pairId: string, result: Vote): RankingRun {
  if (!run.pair || run.pair.id !== pairId) throw new Error('This comparison is stale or already decided.');
  if (result !== 'left' && result !== 'right' && result !== 'both') throw new Error('Choose left, right, or both.');
  const pair = run.pair;
  const next: RankingRun = {
    ...run,
    state: structuredClone(run.state),
    outcomes: { ...run.outcomes },
    revision: run.revision + 1,
    history: appendDecision(run.history, { pair: { ...pair }, result }),
  };
  writeOutcome(next, pair.left, pair.right, result === 'both' ? 2 : result === 'left' ? 1 : 3);
  if (next.state.kind === 'winner-stays') voteWinner(next.state, pair, result);
  else if (next.state.kind === 'adaptive') voteAdaptive(next.state, pair, result);
  else votePairwise(next, next.state, pair, result);
  advance(next);
  return next;
}

export function decide(run: RankingRun, pairId: string, result: Vote): RankingRun {
  return apply(run, pairId, result);
}

/** Replay trades infrequent undo work for compact, durable, complete history. */
export function undo(run: RankingRun): RankingRun {
  if (run.history.length === 0) return run;
  let restored = createRun(run.candidates, run.mode, run.seed, run.randomize);
  let index = 0;
  for (const event of decisions(run.history)) {
    if (++index === run.history.length) break;
    restored = apply(restored, restored.pair!.id, event.result);
  }
  restored.revision = run.revision + 1;
  if (restored.pair) {
    const pair = restored.pair;
    // Mint a new token while restoring exact sides, phase, and carried identity.
    restored.pair = {
      ...pair,
      id: pairToken(restored, pair.left, pair.right, pair.phase),
    };
  }
  return restored;
}

export function progress(run: RankingRun): { done: number; total?: number; remaining?: number; phase: string } {
  const done = run.history.length;
  const phase = run.pair?.phase ?? 'complete';
  if (!run.pair) return { done, total: done, remaining: 0, phase };
  if (run.mode === 'full-pairwise') {
    const total = estimate(run.order.length, run.mode).baseline;
    return { done, total, remaining: total - done, phase };
  }
  return { done, phase };
}

function sameData(first: unknown, second: unknown): boolean {
  if (first === second) return true;
  if (!first || !second || typeof first !== 'object' || typeof second !== 'object') return false;
  if (Array.isArray(first) !== Array.isArray(second)) return false;
  const a = Object.keys(first).sort();
  const b = Object.keys(second).sort();
  return a.length === b.length && a.every((key, i) => key === b[i] && sameData(
    (first as Record<string, unknown>)[key], (second as Record<string, unknown>)[key],
  ));
}

/** Validate persisted data against replay, rather than trusting a scheduler snapshot. */
export function assertValidRun(value: unknown): asserts value is RankingRun {
  const fail = () => { throw new Error('The saved ranking is invalid or uses an unsupported version.'); };
  if (!value || typeof value !== 'object') fail();
  const saved = value as RankingRun;
  if (saved.schemaVersion !== 1 || saved.algorithmVersion !== 1 || saved.prngVersion !== 'mulberry32-v1' ||
      typeof saved.randomize !== 'boolean' || !Number.isSafeInteger(saved.revision) || saved.revision < 0 ||
      !saved.history || !Array.isArray(saved.history.chunks) || !Number.isSafeInteger(saved.history.length) ||
      saved.history.length < 0 || saved.history.length > 250_000 || saved.revision < saved.history.length) fail();
  let replay: RankingRun;
  try { replay = createRun(saved.candidates, saved.mode, saved.seed, saved.randomize); } catch { return fail(); }
  if (saved.seed !== replay.seed) fail();
  let count = 0;
  for (let chunkIndex = 0; chunkIndex < saved.history.chunks.length; chunkIndex++) {
    const chunk = saved.history.chunks[chunkIndex];
    if (!Array.isArray(chunk) || !chunk.length || chunk.length > 128 ||
        (chunkIndex < saved.history.chunks.length - 1 && chunk.length !== 128)) fail();
    for (const event of chunk) {
      const expected = replay.pair;
      if (!expected || !event || !event.pair || typeof event.pair.id !== 'string' ||
          event.pair.left !== expected.left || event.pair.right !== expected.right ||
          event.pair.phase !== expected.phase || event.pair.carried !== expected.carried) fail();
      try { replay = apply(replay, replay.pair!.id, event.result); } catch { return fail(); }
      if (++count > saved.history.length) fail();
    }
  }
  if (count !== saved.history.length || !sameData(saved.order, replay.order) || !sameData(saved.indices, replay.indices) ||
      !sameData(saved.state, replay.state) || !sameData(saved.outcomes, replay.outcomes)) fail();
  if (replay.pair) {
    replay.revision = saved.revision;
    const expected = { ...replay.pair, id: pairToken(replay, replay.pair.left, replay.pair.right, replay.pair.phase) };
    if (!sameData(saved.pair, expected)) fail();
  } else if (saved.pair !== null) fail();
}
