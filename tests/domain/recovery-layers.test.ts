import { readFileSync, writeFileSync } from 'node:fs';
import { expect, test } from 'vitest';
import { createRun, decide, getPair, getResult, undo, assertValidRun, type RankingRun } from '../../src/domain/ranking';
import { moveReview, undoReview, type ReviewState } from '../../src/components/review';

// Shared with the native recovery tests. Regenerate intentionally with
// UPDATE_RECOVERY_FIXTURE=1; normal tests prove the checked fixture still comes
// from the application reducers, rather than a second native ranking engine.
test('recovery layers retain both application Undo branches', () => {
  let run = createRun(['a', 'b', 'c', 'd'], 'winner-stays', 42, false);
  let penultimate: RankingRun = run;
  for (let steps = 0; getPair(run); steps++) {
    if (steps > 100) throw new Error('Fixture failed to terminate');
    penultimate = run;
    run = decide(run, getPair(run)!.id, 'left');
  }
  const result = getResult(run);
  const review: ReviewState = { computed: result.order, order: result.order, bands: result.bands.filter(band => band.length > 1), picks: [], manual: [], revision: 1, history: [] };
  const edited = moveReview(review, [review.order.at(-1)!], 1);
  const fixture = {
    durable: { run: penultimate, review: null, archivedRuns: [], screen: 'arena' },
    pendingEvent: { run, review, archivedRuns: [], screen: 'review' },
    pendingProposal: { run, review: edited, archivedRuns: [], screen: 'review' },
    undoReview: { run, review: undoReview(edited), archivedRuns: [], screen: 'review' },
    undoComparison: { run: undo(run), review: null, archivedRuns: [{ run, review: edited, date: '2026-09-29T00:00:00.000Z' }], screen: 'arena' },
  };
  for (const state of Object.values(fixture)) assertValidRun(state.run);
  expect(edited.history).toHaveLength(1);
  expect(fixture.undoReview.review.order).toEqual(review.order);
  expect(fixture.undoComparison.run.history.length).toBe(penultimate.history.length);
  const path = new URL('../fixtures/recovery-layers.json', import.meta.url);
  if (process.env.UPDATE_RECOVERY_FIXTURE === '1') writeFileSync(path, `${JSON.stringify(fixture, null, 2)}\n`);
  expect(JSON.parse(readFileSync(path, 'utf8'))).toEqual(fixture);
});
