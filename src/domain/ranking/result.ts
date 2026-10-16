import { components, orderBucket } from './graph';
import { observations } from './storage';
import type { RankingResult, RankingRun } from './types';

export function getResult(run: RankingRun): RankingResult {
  if (run.pair) throw new Error('Finish the comparisons before opening the ranking result.');
  const ambiguities: RankingResult['ambiguities'] = [];
  const edges = new Map(run.order.map(id => [id, [] as string[]]));
  for (const { first, second, result } of observations(run)) {
    if (result === 2) ambiguities.push({ kind: 'draw', ids: [first, second] });
    else edges.get(result === 1 ? first : second)!.push(result === 1 ? second : first);
  }
  for (const group of components(run.order, edges, run.indices)) {
    if (group.length > 1) ambiguities.push({ kind: 'cycle', ids: group });
  }
  let bands: string[][];
  if (run.state.kind === 'full-pairwise') {
    const scoreBuckets = new Map<number, string[]>();
    for (const id of run.order) {
      const score = run.state.scores[run.indices[id]];
      scoreBuckets.set(score, [...(scoreBuckets.get(score) ?? []), id]);
    }
    const order: string[] = [];
    for (const score of [...scoreBuckets.keys()].sort((a, b) => b - a)) {
      const bucket = orderBucket(scoreBuckets.get(score)!, edges, run.indices);
      order.push(...bucket.order);
      for (const ids of bucket.incomparable) ambiguities.push({ kind: 'incomparable', ids });
    }
    // Equal aggregate scores and overlapping draw edges do not imply equivalence.
    bands = order.map(id => [id]);
  } else {
    bands = run.state.bands.map(band => band.slice());
    for (const ids of bands) if (ids.length > 1) ambiguities.push({ kind: 'provisional-band', ids: ids.slice() });
    if (run.state.kind === 'adaptive') {
      for (const task of run.state.refinements) {
        task.scores.forEach((score, index) => {
          if (score === (task.anchorStronger ? 2 : 0)) ambiguities.push({ kind: 'anchor-conflict', ids: [task.members[index], task.anchor] });
        });
      }
    }
  }
  return { order: bands.flat(), bands, ambiguities };
}
