import { estimate } from './estimates';
import type { RankingRun } from './types';

/** Measured stage progress; variable-length modes have no fixed decision total. */
export function rankingStageProgress(run: RankingRun): {
  label: string; done: number; total: number; note?: string;
} {
  const count = run.candidates.length;
  if (!run.pair) return { label: 'Ranking complete', done: count || 1, total: count || 1 };
  const state = run.state;
  if (state.kind === 'full-pairwise') {
    return { label: 'Completed comparisons', done: run.history.length, total: estimate(count, run.mode).baseline || 1 };
  }
  if (state.kind === 'adaptive') {
    if (state.phase === 'placement') {
      return {
        label: 'Images placed', done: state.bands.reduce((total, band) => total + band.length, 0), total: count,
        note: 'Refinement may follow after all images are placed.',
      };
    }
    const scores = state.refinements.flatMap(task => task.scores);
    return { label: 'Refinement checks completed', done: scores.filter(score => score !== null).length, total: scores.length || 1 };
  }
  return {
    label: 'Images ranked', done: state.bands.reduce((total, band) => total + band.length, 0), total: count,
    note: 'Each winning band advances the bar when its comparisons finish.',
  };
}
