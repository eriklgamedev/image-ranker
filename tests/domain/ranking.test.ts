import { describe, expect, it } from 'vitest';
import { assertValidRun, createRun, decide, estimate, getPair, getResult, progress, undo } from '../../src/domain/ranking';
import type { RankingMode, RankingRun, Vote } from '../../src/domain/ranking';
import { indexedPair, pairIndex } from '../../src/domain/ranking/storage';
import { writeOutcome } from '../../src/domain/ranking/storage';
import { advanceAdaptive } from '../../src/domain/ranking/adaptive';

const modes: RankingMode[] = ['winner-stays', 'adaptive', 'full-pairwise'];
const choices: Vote[] = ['left', 'right', 'both'];
const ids = (n: number) => Array.from({ length: n }, (_, i) => `image-${i}`);

function finish(run: RankingRun, choice: (run: RankingRun) => Vote): RankingRun {
  const bound = estimate(run.order.length, run.mode).maximum;
  while (getPair(run)) {
    expect(run.history.length).toBeLessThan(bound);
    const pair = getPair(run)!;
    expect(pair.left).not.toBe(pair.right);
    expect(run.order).toContain(pair.left);
    expect(run.order).toContain(pair.right);
    run = decide(run, pair.id, choice(run));
  }
  return run;
}

function assertConservation(run: RankingRun) {
  const result = getResult(run);
  expect(result.order.slice().sort()).toEqual(run.candidates.slice().sort());
  expect(new Set(result.order).size).toBe(run.candidates.length);
  expect(result.bands.flat()).toEqual(result.order);
  expect(progress(run)).toEqual({ done: run.history.length, total: run.history.length, remaining: 0, phase: 'complete' });
}

function visualPair(run: RankingRun) {
  const pair = getPair(run);
  if (!pair) return null;
  const { id: _token, ...visual } = pair;
  return visual;
}

describe.each(modes)('%s', mode => {
  it.each([0, 1])('finishes %i candidates without inventing a comparison', n => {
    const run = createRun(ids(n), mode, 1);
    expect(getPair(run)).toBeNull();
    assertConservation(run);
    expect(undo(run)).toBe(run);
  });

  it.each(choices)('terminates all-%s choices while conserving every candidate', result => {
    for (const n of [2, 3, 4, 11, 25]) {
      const run = finish(createRun(ids(n), mode, 78), () => result);
      assertConservation(run);
    }
  });

  it('recovers a strict preference order without ties', () => {
    const input = ids(14);
    const preference = new Map(input.map((id, i) => [id, i]));
    const run = finish(createRun(input, mode, 679), state => preference.get(state.pair!.left)! < preference.get(state.pair!.right)! ? 'left' : 'right');
    expect(getResult(run).order).toEqual(input);
    if (mode !== 'adaptive') expect(run.history.length).toBe(91);
  });

  it.each([3, 4])('exhaustively terminates every decision branch through %i candidates', n => {
    let terminal = 0;
    const visit = (run: RankingRun) => {
      const pair = getPair(run);
      if (!pair) {
        assertConservation(run);
        terminal++;
        return;
      }
      expect(run.history.length).toBeLessThan(estimate(n, mode).maximum);
      for (const choice of choices) visit(decide(run, pair.id, choice));
    };
    visit(createRun(ids(n), mode, 2, false));
    expect(terminal).toBeGreaterThan(8);
  });

  it('is immutable, restores exact visual state on undo, and blocks stale branch tokens', () => {
    const initial = createRun(ids(7), mode, 95);
    let run = initial;
    for (let i = 0; i < 4; i++) {
      const snapshot = JSON.stringify(run);
      const next = decide(run, run.pair!.id, choices[i % 3]);
      expect(JSON.stringify(run)).toBe(snapshot);
      const restored = undo(next);
      expect(visualPair(restored)).toEqual(visualPair(run));
      expect(restored.state).toEqual(run.state);
      expect(restored.outcomes).toEqual(run.outcomes);
      expect(restored.history.length).toBe(run.history.length);
      expect(() => decide(restored, run.pair!.id, 'left')).toThrow(/stale/);
      run = next;
    }
    expect(initial.history.length).toBe(0);
  });

  it('preserves exact JSON resume and validates undo branches by retained replay', () => {
    let run = createRun(ids(8), mode, -777);
    for (let i = 0; i < 6; i++) run = decide(run, run.pair!.id, choices[i % 3]);
    run = decide(undo(run), undo(run).pair!.id, 'both');
    const loaded: unknown = JSON.parse(JSON.stringify(run));
    assertValidRun(loaded);
    expect(loaded).toEqual(run);
    expect(getPair(loaded)).toEqual(getPair(run));
    const completed = finish(loaded, () => 'right');
    assertValidRun(JSON.parse(JSON.stringify(completed)));
    expect(getResult(completed)).toEqual(getResult(finish(run, () => 'right')));
  });

  it('rejects corrupt saved state and stale or invalid decisions', () => {
    const run = createRun(ids(4), mode, 123);
    expect(() => decide(run, 'wrong-pair', 'left')).toThrow(/stale/);
    expect(() => decide(run, run.pair!.id, 'skip' as Vote)).toThrow(/Choose/);
    const next = decide(run, run.pair!.id, 'both');
    expect(() => decide(next, run.pair!.id, 'left')).toThrow(/stale/);
    const corrupt = JSON.parse(JSON.stringify(next));
    corrupt.order.reverse();
    expect(() => assertValidRun(corrupt)).toThrow(/saved ranking/);
    const badVote = JSON.parse(JSON.stringify(next));
    badVote.history.chunks[0][0].pair.left = 'missing';
    expect(() => assertValidRun(badVote)).toThrow(/saved ranking/);
    expect(() => getResult(run)).toThrow(/Finish/);
  });

  it('maintains deterministic seeded outcomes over generated mixed fixtures', () => {
    for (let seed = 0; seed < 20; seed++) {
      const input = ids(2 + seed % 10);
      const policy = (run: RankingRun) => choices[(run.history.length * 13 + seed * 7) % 3];
      const first = finish(createRun(input, mode, seed), policy);
      const second = finish(createRun(input, mode, seed), policy);
      expect(first).toEqual(second);
      assertConservation(first);
    }
  });
});

describe('pair scheduling and ranking semantics', () => {
  it('preserves an explicitly provided path order when randomization is disabled', () => {
    const input = ['uuid-z', 'uuid-a', 'uuid-b'];
    expect(createRun(input, 'adaptive', 42, false).order).toEqual(input);
    expect(createRun(ids(20), 'adaptive', 42).order).toEqual(createRun(ids(20), 'adaptive', 42).order);
    expect(createRun(ids(20), 'adaptive', 42).order).not.toEqual(createRun(ids(20), 'adaptive', 43).order);
  });

  it('indexes triangular pairs without collisions or off-by-one rows', () => {
    for (let n = 2; n <= 50; n++) {
      const pairs = new Set<number>();
      for (let first = 0; first < n; first++) for (let second = first + 1; second < n; second++) {
        const index = pairIndex(n, first, second);
        expect(indexedPair(n, index)).toEqual([first, second]);
        expect(pairIndex(n, second, first)).toBe(index);
        pairs.add(index);
      }
      expect(pairs.size).toBe(n * (n - 1) / 2);
      expect(Math.max(...pairs)).toBe(pairs.size - 1);
    }
  });

  it.each(['winner-stays', 'full-pairwise'] as RankingMode[])('keeps the carried winner on the same side in %s', mode => {
    let run = createRun(ids(6), mode, 4, false);
    for (let i = 0; i < 4; i++) {
      const previous = run.pair!;
      const selected: Vote = i === 0 ? 'right' : previous.left === 'image-1' ? 'left' : 'right';
      const winner = previous[selected];
      run = decide(run, previous.id, selected);
      expect(run.pair!.carried).toBe(winner);
      expect(run.pair![selected]).toBe(winner);
    }
  });

  it('never presents a placed Winner-Stays champion in later passes', () => {
    const run = finish(createRun(['A', 'B', 'C'], 'winner-stays', 1, false), current => current.pair!.left === 'A' ? 'left' : current.pair!.right === 'A' ? 'right' : 'left');
    const secondPass = run.history.chunks.flat().filter(event => event.pair.phase === 'champion-pass-2');
    expect(secondPass).toHaveLength(1);
    expect([secondPass[0].pair.left, secondPass[0].pair.right]).not.toContain('A');
    expect(getResult(run).order[0]).toBe('A');
  });

  it('allows a later challenger to split prior Winner-Stays draw contenders', () => {
    let run = createRun(['A', 'B', 'C'], 'winner-stays', 1, false);
    run = decide(run, run.pair!.id, 'both');
    run = decide(run, run.pair!.id, run.pair!.left === 'C' ? 'left' : 'right');
    run = decide(run, run.pair!.id, run.pair!.left === 'A' ? 'left' : 'right');
    run = finish(run, () => 'both');
    expect(getResult(run).bands).toEqual([['A'], ['B', 'C']]);
  });

  it('covers each full pair exactly once even with alternating draws and winners', () => {
    const seen = new Set<string>();
    const run = finish(createRun(ids(30), 'full-pairwise', 394), state => {
      const pair = state.pair!;
      const key = [pair.left, pair.right].sort().join(':');
      expect(seen.has(key)).toBe(false);
      seen.add(key);
      expect(progress(state).remaining).toBe(435 - seen.size + 1);
      return choices[state.history.length % 3];
    });
    expect(run.history.length).toBe(435);
    expect(seen.size).toBe(435);
    assertConservation(run);
  });

  it('queues both draw participants fairly in Full Pairwise', () => {
    let run = createRun(['A', 'B', 'C', 'D'], 'full-pairwise', 1, false);
    run = decide(run, run.pair!.id, 'both');
    expect(run.pair!.carried).toBe('A');
    run = decide(run, run.pair!.id, 'both');
    expect(run.pair!.carried).toBe('B');
  });

  it('partitions a refinement band identically with cached and newly collected anchor evidence', () => {
    function fixture(cacheLastObservation: boolean) {
      const run = createRun(['A', 'B', 'C'], 'adaptive', 1, false);
      if (run.state.kind !== 'adaptive') throw new Error('Wrong fixture mode');
      run.state.bands = [['A'], ['B', 'C']];
      run.state.insertion = null;
      run.state.next = 3;
      writeOutcome(run, 'B', 'A', 3);
      if (cacheLastObservation) writeOutcome(run, 'C', 'A', 1);
      run.pair = advanceAdaptive(run, run.state);
      return run;
    }
    const cached = fixture(true);
    expect(cached.pair).toBeNull();
    const uncached = fixture(false);
    expect(uncached.pair!.phase).toBe('refinement');
    const answered = decide(uncached, uncached.pair!.id, uncached.pair!.left === 'C' ? 'left' : 'right');
    expect(getResult(cached)).toEqual(getResult(answered));
    expect(getResult(cached).bands).toEqual([['A'], ['C'], ['B']]);
    expect(getResult(cached).ambiguities).toContainEqual({ kind: 'anchor-conflict', ids: ['C', 'A'] });
  });

  it('retains an all-draw adaptive band when no external refinement anchor exists', () => {
    const run = finish(createRun(['A', 'B', 'C'], 'adaptive', 1, false), () => 'both');
    expect(getResult(run).bands).toEqual([['A', 'B', 'C']]);
    expect(run.history.length).toBe(2);
  });

  it('keeps cycles distinct from draw bands and provides stable total ordering', () => {
    const beats: Record<string, string> = { A: 'B', B: 'C', C: 'A' };
    const run = finish(createRun(['A', 'B', 'C'], 'full-pairwise', 1, false), current => beats[current.pair!.left] === current.pair!.right ? 'left' : 'right');
    expect(getResult(run)).toEqual({ order: ['A', 'B', 'C'], bands: [['A'], ['B'], ['C']], ambiguities: [{ kind: 'cycle', ids: ['A', 'B', 'C'] }] });
  });

  it('does not infer a transitive draw from two overlapping observed draws', () => {
    const run = finish(createRun(['A', 'B', 'C'], 'full-pairwise', 1, false), current => {
      const pair = current.pair!;
      if ([pair.left, pair.right].includes('B')) return 'both';
      return pair.left === 'A' ? 'left' : 'right';
    });
    const result = getResult(run);
    expect(result.order).toEqual(['A', 'B', 'C']);
    expect(result.bands).toEqual([['A'], ['B'], ['C']]);
    expect(result.ambiguities.filter(item => item.kind === 'draw')).toHaveLength(2);
    expect(result.ambiguities.some(item => item.ids.length === 3 && item.kind === 'provisional-band')).toBe(false);
  });

  it('honors decisive head-to-head evidence within equal-score buckets before seeded order', () => {
    const wins = new Set(['A:B', 'A:C', 'B:C', 'B:D', 'C:D', 'D:A']);
    const run = finish(createRun(['B', 'A', 'D', 'C'], 'full-pairwise', 1, false), current => wins.has(`${current.pair!.left}:${current.pair!.right}`) ? 'left' : 'right');
    expect(getResult(run).order).toEqual(['A', 'B', 'C', 'D']);
  });

  it('uses chunked history sharing instead of cloning prior events', () => {
    let run = createRun(ids(20), 'full-pairwise', 0);
    for (let i = 0; i < 129; i++) run = decide(run, run.pair!.id, 'left');
    const next = decide(run, run.pair!.id, 'right');
    expect(next.history.chunks[0]).toBe(run.history.chunks[0]);
    expect(next.history.chunks[1][0]).toBe(run.history.chunks[1][0]);
    expect(next.history.chunks[1]).not.toBe(run.history.chunks[1]);
    expect(JSON.stringify(next).length).toBeLessThan(100_000);
  });
});

describe('workload limits', () => {
  it('reports proven comparison bounds rather than an unexplained adaptive multiplier', () => {
    expect(estimate(10, 'full-pairwise')).toEqual({ baseline: 45, maximum: 45, exact: true });
    expect(estimate(10, 'winner-stays')).toEqual({ baseline: 45, maximum: 165, exact: false });
    expect(estimate(4, 'adaptive')).toEqual({ baseline: 5, maximum: 9, exact: false });
    expect(estimate(10_000, 'full-pairwise').baseline).toBe(49_995_000);
  });

  it('rejects unsupported counts, IDs, seeds, modes, and unbounded sessions', () => {
    expect(() => estimate(-1, 'adaptive')).toThrow();
    expect(() => estimate(1.5, 'adaptive')).toThrow();
    expect(() => estimate(Number.MAX_SAFE_INTEGER, 'winner-stays')).toThrow(/arithmetic/);
    expect(() => estimate(10, 'invalid' as RankingMode)).toThrow();
    expect(() => createRun(['A', 'A'], 'adaptive', 1)).toThrow(/distinct/);
    expect(() => createRun([''], 'adaptive', 1)).toThrow(/nonempty/);
    expect(() => createRun(ids(2), 'adaptive', NaN)).toThrow(/seed/);
    expect(() => createRun(ids(1000), 'full-pairwise', 1)).toThrow(/Adaptive/);
    expect(() => createRun(ids(10_001), 'adaptive', 1)).toThrow(/10,000/);
    for (const value of [null, {}, { schemaVersion: 2 }]) expect(() => assertValidRun(value)).toThrow();
  });
});
