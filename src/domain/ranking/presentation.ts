import type { Carry, Comparison, RankingRun } from './types';

export function pairToken(run: RankingRun, left: string, right: string, phase: string): string {
  return `${run.mode}:${run.seed}:${run.revision}:${run.indices[left]}:${run.indices[right]}:${phase}`;
}

export function present(run: RankingRun, first: string, second: string, phase: string, carry: Carry | null = null): Comparison {
  const held = carry && (carry.id === first || carry.id === second) ? carry : null;
  const other = held?.id === first ? second : first;
  const left = held ? (held.side === 'left' ? held.id : other) : run.history.length % 2 === 0 ? first : second;
  const right = left === first ? second : first;
  return {
    id: pairToken(run, left, right, phase),
    left,
    right,
    phase,
    ...(held ? { carried: held.id } : {}),
  };
}

export function carryFor(pair: Comparison, id: string): Carry {
  return { id, side: pair.left === id ? 'left' : 'right' };
}
