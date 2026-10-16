export interface ReviewSnapshot {
  order: string[];
  picks: string[];
  bands: string[][];
  manual: string[];
}
export interface ReviewState extends ReviewSnapshot {
  computed: string[];
  revision: number;
  history: ReviewSnapshot[];
}

export function changeReview(
  review: ReviewState,
  patch: Partial<ReviewSnapshot>,
): ReviewState {
  return {
    ...review,
    ...patch,
    revision: review.revision + 1,
    history: [
      ...review.history,
      {
        order: review.order,
        picks: review.picks,
        bands: review.bands,
        manual: review.manual,
      },
    ],
  };
}
export function undoReview(review: ReviewState): ReviewState {
  const previous = review.history.at(-1);
  return previous
    ? {
        ...review,
        ...previous,
        revision: review.revision + 1,
        history: review.history.slice(0, -1),
      }
    : review;
}
/** Removing members splits a band; its surviving disjoint segments never become a new tie. */
function detach(bands: string[][], removed: Set<string>): string[][] {
  const result: string[][] = [];
  for (const band of bands) {
    let segment: string[] = [];
    for (const id of band) {
      if (removed.has(id)) {
        if (segment.length > 1) result.push(segment);
        segment = [];
      } else segment.push(id);
    }
    if (segment.length > 1) result.push(segment);
  }
  return result;
}
/** An inserted block can split an unmoved band as well as its own origin band. */
function splitAtOrderGaps(bands: string[][], order: string[]): string[][] {
  const positions = new Map(order.map((id, index) => [id, index]));
  const result: string[][] = [];
  for (const band of bands) {
    let segment: string[] = [];
    let previous: number | undefined;
    for (const id of band) {
      const position = positions.get(id);
      if (
        segment.length > 0 &&
        (previous === undefined || position !== previous + 1)
      ) {
        if (segment.length > 1) result.push(segment);
        segment = [];
      }
      segment.push(id);
      previous = position;
    }
    if (segment.length > 1) result.push(segment);
  }
  return result;
}
/** Rank is the final one-based position of the first moved item. Hidden items retain their relative order. */
export function moveReview(
  review: ReviewState,
  ids: string[],
  rank: number,
): ReviewState {
  const selected = new Set(ids),
    moving = review.order.filter((id) => selected.has(id));
  if (!moving.length) return review;
  const rest = review.order.filter((id) => !selected.has(id));
  const index = Math.max(0, Math.min(rest.length, Math.trunc(rank) - 1));
  const order = [...rest.slice(0, index), ...moving, ...rest.slice(index)];
  if (order.every((id, i) => id === review.order[i])) return review;
  const bands = splitAtOrderGaps(detach(review.bands, selected), order);
  return changeReview(review, {
    order,
    bands,
    manual: [...new Set([...review.manual, ...moving])],
  });
}
export function compareReview(
  review: ReviewState,
  left: string,
  right: string,
  result: "left" | "right" | "both",
): ReviewState {
  const a = review.order.indexOf(left),
    b = review.order.indexOf(right);
  if (a < 0 || b < 0 || Math.abs(a - b) !== 1)
    throw new Error(
      "Recompare requires two adjacent images in the full ranking.",
    );
  const pair = new Set([left, right]);
  const bands = detach(review.bands, pair);
  const order = [...review.order];
  if (result === "both") bands.push(a < b ? [left, right] : [right, left]);
  else {
    order[Math.min(a, b)] = result === "left" ? left : right;
    order[Math.max(a, b)] = result === "left" ? right : left;
  }
  return changeReview(review, {
    order,
    bands,
    manual: [...new Set([...review.manual, left, right])],
  });
}

export function rangeSelection(
  selected: string[],
  visible: string[],
  anchor: string | null,
  id: string,
): string[] {
  const next = new Set(selected),
    target = !next.has(id),
    a = anchor ? visible.indexOf(anchor) : -1,
    b = visible.indexOf(id);
  if (b < 0) return selected;
  for (const item of visible.slice(
    a < 0 ? b : Math.min(a, b),
    a < 0 ? b + 1 : Math.max(a, b) + 1,
  ))
    target ? next.add(item) : next.delete(item);
  return [...next];
}
