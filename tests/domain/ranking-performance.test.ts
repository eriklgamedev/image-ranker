import { expect, it } from 'vitest';
import { assertValidRun, createRun, decide, getResult } from '../../src/domain/ranking';

it('retains a 100-image full-pairwise run in a linear-size serializable log', () => {
  const start = performance.now();
  let run = createRun(Array.from({ length: 100 }, (_, i) => `image-${i}`), 'full-pairwise', 19);
  while (run.pair) run = decide(run, run.pair.id, run.history.length % 3 === 0 ? 'both' : 'left');
  const completeMs = performance.now() - start;
  const serialized = JSON.stringify(run);
  const replayStart = performance.now();
  assertValidRun(JSON.parse(serialized));
  const replayMs = performance.now() - replayStart;
  expect(run.history.length).toBe(4_950);
  expect(getResult(run).order).toHaveLength(100);
  expect(serialized.length).toBeLessThan(run.history.length * 300);
  console.info(`100-image Full Pairwise: 4,950 decisions, ${serialized.length} JSON characters, ${completeMs.toFixed(1)} ms execution, ${replayMs.toFixed(1)} ms validation replay.`);
});

it('starts and advances the supported 10,000-image Adaptive configuration', () => {
  const start = performance.now();
  const initial = createRun(Array.from({ length: 10_000 }, (_, i) => `image-${i}`), 'adaptive', 9);
  const run = decide(initial, initial.pair!.id, 'left');
  expect(run.order).toHaveLength(10_000);
  expect(run.history.length).toBe(1);
  expect(run.pair).not.toBeNull();
  console.info(`10,000-image Adaptive initialization and first vote: ${(performance.now() - start).toFixed(1)} ms; ${JSON.stringify(run).length} JSON characters.`);
});
