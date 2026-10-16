import { carryFor, present } from './presentation';
import type { Comparison, RankingRun, Vote, WinnerState } from './types';

export function startWinner(order: string[]): WinnerState {
  return { kind: 'winner-stays', live: order.slice(0, 1), queue: order.slice(1), lower: [], challenger: null, contender: 0, bands: [], pass: 1, carry: null };
}

export function advanceWinner(run: RankingRun, state: WinnerState): Comparison | null {
  // Every iteration consumes a challenger, places a band, or produces a decision.
  while (state.live.length || state.challenger || state.queue.length) {
    if (state.challenger === null) {
      state.challenger = state.queue.shift() ?? null;
      state.contender = 0;
      if (state.challenger === null) {
        state.bands.push(state.live.slice().sort((a, b) => run.indices[a] - run.indices[b]));
        state.live = state.lower.slice(0, 1);
        state.queue = state.lower.slice(1);
        state.lower = [];
        state.carry = null;
        state.pass++;
        continue;
      }
    }
    if (state.contender >= state.live.length) {
      // The surviving challenger is first next time so its screen side can carry.
      state.live.unshift(state.challenger);
      state.challenger = null;
      continue;
    }
    return present(run, state.live[state.contender], state.challenger, `champion-pass-${state.pass}`, state.carry);
  }
  return null;
}

export function voteWinner(state: WinnerState, pair: Comparison, result: Vote): void {
  const challenger = state.challenger!;
  const incumbent = state.live[state.contender];
  if (result === 'both') {
    state.contender++;
    state.carry = carryFor(pair, challenger);
    return;
  }
  const winner = pair[result];
  state.carry = carryFor(pair, winner);
  if (winner === challenger) {
    state.lower.push(incumbent);
    state.live.splice(state.contender, 1);
  } else {
    state.lower.push(challenger);
    state.challenger = null;
    // A contender that defeated a challenger remains the first incumbent.
    state.live.splice(state.contender, 1);
    state.live.unshift(incumbent);
  }
}
