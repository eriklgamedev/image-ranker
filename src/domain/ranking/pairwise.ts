import { carryFor, present } from './presentation';
import { indexedPair, pairIndex, readOutcome } from './storage';
import type { Comparison, PairwiseState, RankingRun, Vote } from './types';

export function startPairwise(order: string[]): PairwiseState {
  return { kind: 'full-pairwise', scores: order.map(() => 0), carry: [], cursor: 0 };
}

export function advancePairwise(run: RankingRun, state: PairwiseState): Comparison | null {
  const n = run.order.length;
  while (state.carry.length) {
    const carry = state.carry.shift()!;
    const first = run.indices[carry.id];
    for (let second = 0; second < n; second++) {
      if (first !== second && !readOutcome(run, pairIndex(n, first, second))) {
        return present(run, carry.id, run.order[second], 'pairwise', carry);
      }
    }
  }
  const total = (n * (n - 1)) / 2;
  while (state.cursor < total && readOutcome(run, state.cursor)) state.cursor++;
  if (state.cursor === total) return null;
  const [first, second] = indexedPair(n, state.cursor);
  return present(run, run.order[first], run.order[second], 'pairwise');
}

export function votePairwise(run: RankingRun, state: PairwiseState, pair: Comparison, result: Vote): void {
  const first = run.indices[pair.left];
  const second = run.indices[pair.right];
  if (result === 'both') {
    state.scores[first]++;
    state.scores[second]++;
    for (const id of [pair.left, pair.right]) {
      if (!state.carry.some(entry => entry.id === id)) state.carry.push(carryFor(pair, id));
    }
  } else {
    const winner = pair[result];
    state.scores[run.indices[winner]] += 2;
    state.carry = [carryFor(pair, winner), ...state.carry.filter(entry => entry.id !== pair.left && entry.id !== pair.right)];
  }
}
