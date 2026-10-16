export type RankingMode = 'winner-stays' | 'adaptive' | 'full-pairwise';
export type Vote = 'left' | 'right' | 'both';
export type Side = 'left' | 'right';

export interface Comparison {
  id: string;
  left: string;
  right: string;
  phase: string;
  carried?: string;
}

export interface Decision {
  pair: Comparison;
  result: Vote;
}

/** Chunking shares older events between immutable states, without nested snapshots. */
export interface DecisionHistory {
  chunks: Decision[][];
  length: number;
}

export interface Carry {
  id: string;
  side: Side;
}

export interface WinnerState {
  kind: 'winner-stays';
  live: string[];
  queue: string[];
  lower: string[];
  challenger: string | null;
  contender: number;
  bands: string[][];
  pass: number;
  carry: Carry | null;
}

export interface Refinement {
  members: string[];
  anchor: string;
  anchorStronger: boolean;
  scores: Array<number | null>;
}

export interface AdaptiveState {
  kind: 'adaptive';
  bands: string[][];
  next: number;
  insertion: { id: string; low: number; high: number } | null;
  phase: 'placement' | 'refinement' | 'complete';
  refinements: Refinement[];
  task: number;
  member: number;
}

export interface PairwiseState {
  kind: 'full-pairwise';
  scores: number[];
  carry: Carry[];
  cursor: number;
}

/** Plain JSON data. Outcomes use 0=unseen, 1=first wins, 2=draw, 3=second wins. */
export interface RankingRun {
  schemaVersion: 1;
  algorithmVersion: 1;
  prngVersion: 'mulberry32-v1';
  mode: RankingMode;
  seed: number;
  randomize: boolean;
  candidates: string[];
  order: string[];
  indices: Record<string, number>;
  outcomes: Record<string, string>;
  state: WinnerState | AdaptiveState | PairwiseState;
  pair: Comparison | null;
  history: DecisionHistory;
  revision: number;
}

export interface RankingResult {
  order: string[];
  bands: string[][];
  ambiguities: Array<{ kind: string; ids: string[] }>;
}
