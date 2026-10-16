import type { RankingMode } from './types';

export const MAX_CANDIDATES = 10_000;
export const MAX_DECISIONS = 250_000;

export function estimate(n: number, mode: RankingMode): { baseline: number; maximum: number; exact: boolean } {
  if (!Number.isSafeInteger(n) || n < 0) throw new Error('Candidate count must be a nonnegative safe integer.');
  let baseline = 0;
  let maximum = 0;
  if (mode === 'adaptive') {
    // Inserting item k into k-1 singleton bands needs at most ceil(log2(k)) decisions.
    // Summing by powers of two avoids a loop proportional to an unchecked input.
    for (let start = 2, depth = 1; start <= n; start = 2 ** depth + 1, depth++) {
      const end = Math.min(n, 2 ** depth);
      baseline += (end - start + 1) * depth;
    }
    maximum = baseline + (n >= 3 ? n : 0);
  } else if (mode === 'winner-stays' || mode === 'full-pairwise') {
    baseline = (n * (n - 1)) / 2;
    maximum = mode === 'full-pairwise' ? baseline : (n * (n - 1) * (n + 1)) / 6;
  } else {
    throw new Error('Unknown ranking mode.');
  }
  if (!Number.isSafeInteger(baseline) || !Number.isSafeInteger(maximum)) {
    throw new Error('Comparison count exceeds safe arithmetic limits. Choose fewer images.');
  }
  return { baseline, maximum, exact: mode === 'full-pairwise' || n <= 2 };
}

export function checkWorkload(n: number, mode: RankingMode): void {
  if (n > MAX_CANDIDATES) throw new Error(`Choose at most ${MAX_CANDIDATES.toLocaleString('en-US')} images per run.`);
  if (estimate(n, mode).maximum > MAX_DECISIONS) {
    throw new Error('This run exceeds the 250,000-decision retained-history budget. Choose Adaptive or fewer images.');
  }
}
