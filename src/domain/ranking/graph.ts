/** Iterative Kosaraju avoids relying on the JS stack for large preference graphs. */
export function components(ids: string[], edges: Map<string, string[]>, indices: Record<string, number>): string[][] {
  const members = new Set(ids);
  const adjacency = new Map(ids.map(id => [id, (edges.get(id) ?? []).filter(next => members.has(next))]));
  const reverse = new Map(ids.map(id => [id, [] as string[]]));
  for (const [from, next] of adjacency) for (const to of next) reverse.get(to)!.push(from);
  const visited = new Set<string>();
  const finished: string[] = [];
  for (const id of ids) {
    if (visited.has(id)) continue;
    const stack: Array<{ id: string; cursor: number }> = [{ id, cursor: 0 }];
    visited.add(id);
    while (stack.length) {
      const current = stack[stack.length - 1];
      const next = adjacency.get(current.id)!;
      if (current.cursor < next.length) {
        const child = next[current.cursor++];
        if (!visited.has(child)) {
          visited.add(child);
          stack.push({ id: child, cursor: 0 });
        }
      } else {
        finished.push(current.id);
        stack.pop();
      }
    }
  }
  visited.clear();
  const result: string[][] = [];
  for (const id of finished.reverse()) {
    if (visited.has(id)) continue;
    const group: string[] = [];
    const stack = [id];
    visited.add(id);
    while (stack.length) {
      const current = stack.pop()!;
      group.push(current);
      for (const next of reverse.get(current)!) {
        if (!visited.has(next)) {
          visited.add(next);
          stack.push(next);
        }
      }
    }
    result.push(group.sort((a, b) => indices[a] - indices[b]));
  }
  return result;
}

/** Winner-before-loser topological order after collapsing cycles. */
export function orderBucket(ids: string[], edges: Map<string, string[]>, indices: Record<string, number>): { order: string[]; incomparable: string[][] } {
  const groups = components(ids, edges, indices);
  const groupOf = new Map(groups.flatMap((group, index) => group.map(id => [id, index] as const)));
  const next = groups.map(() => new Set<number>());
  const indegree = groups.map(() => 0);
  for (const [from, children] of edges) {
    const source = groupOf.get(from);
    if (source === undefined) continue;
    for (const child of children) {
      const target = groupOf.get(child);
      if (target !== undefined && source !== target && !next[source].has(target)) {
        next[source].add(target);
        indegree[target]++;
      }
    }
  }
  const ready = indegree.flatMap((value, index) => value === 0 ? [index] : []);
  const order: string[] = [];
  const incomparable: string[][] = [];
  // Store a bounded annotation per bucket, not O(n²) combinations of draw edges.
  const unordered = new Set<string>();
  while (ready.length) {
    ready.sort((a, b) => indices[groups[a][0]] - indices[groups[b][0]]);
    if (ready.length > 1) for (const index of ready) for (const id of groups[index]) unordered.add(id);
    const index = ready.shift()!;
    order.push(...groups[index]);
    for (const child of next[index]) if (--indegree[child] === 0) ready.push(child);
  }
  if (unordered.size) incomparable.push([...unordered].sort((a, b) => indices[a] - indices[b]));
  return { order, incomparable };
}
