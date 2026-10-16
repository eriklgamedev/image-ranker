import { present } from './presentation';
import { observation } from './storage';
import type { AdaptiveState, Comparison, RankingRun, Vote } from './types';

export function startAdaptive(order: string[]): AdaptiveState {
  return { kind: 'adaptive', bands: order.length ? [[order[0]]] : [], next: Math.min(1, order.length), insertion: null, phase: 'placement', refinements: [], task: 0, member: 0 };
}

function freezeRefinement(run: RankingRun, state: AdaptiveState): void {
  state.phase = 'refinement';
  state.refinements = state.bands.flatMap((members, index, bands) => {
    if (members.length < 2 || bands.length < 2) return [];
    const anchor = bands[index > 0 ? index - 1 : index + 1][0];
    return [{ members: members.slice(), anchor, anchorStronger: index > 0, scores: members.map(id => observation(run, id, anchor)) }];
  });
}

export function advanceAdaptive(run: RankingRun, state: AdaptiveState): Comparison | null {
  while (state.phase !== 'complete') {
    if (state.phase === 'placement') {
      if (!state.insertion) {
        if (state.next === run.order.length) {
          freezeRefinement(run, state);
          continue;
        }
        state.insertion = { id: run.order[state.next++], low: 0, high: state.bands.length };
      }
      const search = state.insertion;
      if (search.low === search.high) {
        state.bands.splice(search.low, 0, [search.id]);
        state.insertion = null;
        continue;
      }
      const midpoint = Math.floor((search.low + search.high) / 2);
      return present(run, state.bands[midpoint][0], search.id, 'placement');
    }
    const task = state.refinements[state.task];
    if (!task) {
      state.phase = 'complete';
      continue;
    }
    state.member = task.scores.findIndex(score => score === null);
    if (state.member >= 0) return present(run, task.members[state.member], task.anchor, 'refinement');

    // Cached observations have exactly the same partition semantics as new votes.
    const index = state.bands.findIndex(band => band.includes(task.members[0]));
    const partitions = [2, 1, 0].map(score => task.members.filter((_, i) => task.scores[i] === score)).filter(band => band.length);
    state.bands.splice(index, 1, ...partitions);
    state.task++;
  }
  return null;
}

export function voteAdaptive(state: AdaptiveState, pair: Comparison, result: Vote): void {
  if (state.phase === 'refinement') {
    const task = state.refinements[state.task];
    task.scores[state.member] = result === 'both' ? 1 : pair[result] === task.members[state.member] ? 2 : 0;
    return;
  }
  const search = state.insertion!;
  const midpoint = Math.floor((search.low + search.high) / 2);
  if (result === 'both') {
    state.bands[midpoint].push(search.id);
    state.insertion = null;
  } else if (pair[result] === search.id) search.high = midpoint;
  else search.low = midpoint + 1;
}
