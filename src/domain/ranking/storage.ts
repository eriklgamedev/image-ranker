import type { Decision, DecisionHistory, RankingRun } from './types';

const HISTORY_CHUNK = 128;
const OUTCOME_PAGE = 1024;

export function appendDecision(history: DecisionHistory, event: Decision): DecisionHistory {
  const chunks = history.chunks.slice();
  const last = chunks.at(-1);
  if (!last || last.length === HISTORY_CHUNK) chunks.push([event]);
  else chunks[chunks.length - 1] = [...last, event];
  return { chunks, length: history.length + 1 };
}

export function* decisions(history: DecisionHistory): Generator<Decision> {
  for (const chunk of history.chunks) yield* chunk;
}

/** Index in an upper triangular matrix, without allocating pair objects. */
export function pairIndex(n: number, first: number, second: number): number {
  const a = Math.min(first, second);
  const b = Math.max(first, second);
  return (a * (2 * n - a - 1)) / 2 + b - a - 1;
}

export function indexedPair(n: number, index: number): [number, number] {
  let low = 0;
  let high = n - 1;
  while (low + 1 < high) {
    const midpoint = Math.floor((low + high) / 2);
    const rowStart = (midpoint * (2 * n - midpoint - 1)) / 2;
    if (rowStart <= index) low = midpoint;
    else high = midpoint;
  }
  const rowStart = (low * (2 * n - low - 1)) / 2;
  return [low, low + index - rowStart + 1];
}

export function readOutcome(run: RankingRun, index: number): number {
  const page = run.outcomes[Math.floor(index / OUTCOME_PAGE)];
  return page ? Number(page[index % OUTCOME_PAGE] ?? '0') : 0;
}

export function writeOutcome(run: RankingRun, first: string, second: string, result: number): void {
  const a = run.indices[first];
  const b = run.indices[second];
  const index = pairIndex(run.order.length, a, b);
  const pageKey = Math.floor(index / OUTCOME_PAGE);
  const offset = index % OUTCOME_PAGE;
  const page = run.outcomes[pageKey] ?? '0'.repeat(OUTCOME_PAGE);
  const normalized = a < b || result === 2 ? result : 4 - result;
  run.outcomes[pageKey] = page.slice(0, offset) + normalized + page.slice(offset + 1);
}

/** Half-points for `member` against `anchor`, or null if they have not met. */
export function observation(run: RankingRun, member: string, anchor: string): number | null {
  const a = run.indices[member];
  const b = run.indices[anchor];
  const result = readOutcome(run, pairIndex(run.order.length, a, b));
  if (!result) return null;
  if (result === 2) return 1;
  return (result === 1) === (a < b) ? 2 : 0;
}

export function* observations(run: RankingRun): Generator<{ first: string; second: string; result: number }> {
  for (const [key, page] of Object.entries(run.outcomes)) {
    for (let offset = 0; offset < page.length; offset++) {
      const result = Number(page[offset]);
      if (!result) continue;
      const [a, b] = indexedPair(run.order.length, Number(key) * OUTCOME_PAGE + offset);
      yield { first: run.order[a], second: run.order[b], result };
    }
  }
}
