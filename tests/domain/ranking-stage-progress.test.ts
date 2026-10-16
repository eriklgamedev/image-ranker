import { describe, expect, it } from 'vitest';
import { createRun, decide, getPair, undo } from '../../src/domain/ranking';
import type { RankingMode } from '../../src/domain/ranking';
import { rankingStageProgress } from '../../src/domain/ranking/progress';

describe('measured ranking stage progress', () => {
  it('measures Adaptive placed images rather than guessing a decision total', () => {
    const run = createRun(['a', 'b', 'c', 'd'], 'adaptive', 1, false);
    expect(rankingStageProgress(run)).toMatchObject({ label: 'Images placed', done: 1, total: 4 });
    const next = decide(run, getPair(run)!.id, 'both');
    expect(rankingStageProgress(next)).toMatchObject({ label: 'Images placed', done: 2, total: 4 });
    expect(rankingStageProgress(undo(next))).toEqual(rankingStageProgress(run));
  });

  it('uses the actual refinement checks, including already-known evidence', () => {
    let run = createRun(['a', 'b', 'c', 'd'], 'adaptive', 1, false);
    run = decide(run, getPair(run)!.id, 'both');
    while (run.pair?.phase === 'placement') {
      const pair = getPair(run)!;
      const result = [pair.left, pair.right].includes('c') && [pair.left, pair.right].includes('d')
        ? 'both' : pair.left === 'c' || pair.left === 'd' ? 'left' : 'right';
      run = decide(run, pair.id, result);
    }
    expect(run.pair?.phase).toBe('refinement');
    const stage = rankingStageProgress(run);
    expect(stage).toEqual({ label: 'Refinement checks completed', done: 3, total: 4 });
    expect(rankingStageProgress(decide(run, getPair(run)!.id, 'both'))).toEqual({ label: 'Ranking complete', done: 4, total: 4 });
  });

  it('reports exact Full Pairwise comparisons and finalized Winner stays images', () => {
    const pairwise = createRun(['a', 'b', 'c', 'd'], 'full-pairwise', 1, false);
    expect(rankingStageProgress(pairwise)).toEqual({ label: 'Completed comparisons', done: 0, total: 6 });
    expect(rankingStageProgress(decide(pairwise, getPair(pairwise)!.id, 'both'))).toEqual({ label: 'Completed comparisons', done: 1, total: 6 });
    expect(rankingStageProgress(createRun(['a', 'b', 'c'], 'winner-stays', 1, false))).toMatchObject({ label: 'Images ranked', done: 0, total: 3 });
  });

  it.each<RankingMode>(['adaptive', 'winner-stays', 'full-pairwise'])('stays bounded, reaches completion, and never changes the %s ranking', mode => {
    let run = createRun(['a', 'b', 'c', 'd', 'e'], mode, 10);
    while (getPair(run)) {
      const before = JSON.stringify(run), stage = rankingStageProgress(run);
      expect(JSON.stringify(run)).toBe(before);
      expect(stage.done).toBeGreaterThanOrEqual(0);
      expect(stage.done).toBeLessThanOrEqual(stage.total);
      expect(stage.total).toBeGreaterThan(0);
      run = decide(run, getPair(run)!.id, run.history.length % 3 === 0 ? 'both' : 'left');
    }
    expect(rankingStageProgress(run)).toEqual({ label: 'Ranking complete', done: 5, total: 5 });
    expect(rankingStageProgress(JSON.parse(JSON.stringify(run)))).toEqual(rankingStageProgress(run));
  });

  it.each([0, 1])('finishes %i images without dividing by zero or inventing decisions', count => {
    const run = createRun(['a'].slice(0, count), 'adaptive', 1);
    expect(rankingStageProgress(run)).toEqual({ label: 'Ranking complete', done: count || 1, total: count || 1 });
    expect(run.history.length).toBe(0);
  });
});
