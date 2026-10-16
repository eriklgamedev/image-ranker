import { describe, it, expect } from "vitest";
import {
  changeReview,
  compareReview,
  moveReview,
  rangeSelection,
  undoReview,
  type ReviewState,
} from "../../src/components/review";
const fixture = (): ReviewState => ({
  computed: ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"],
  order: ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"],
  picks: ["b", "i"],
  bands: [["h", "i", "j"]],
  manual: [],
  revision: 1,
  history: [],
});
describe("authoritative review edits", () => {
  it("moves rank nine to two and undoes every review field after serialization", () => {
    const initial = fixture(),
      moved = moveReview(initial, ["i"], 2);
    expect(moved.order).toEqual([
      "a",
      "i",
      "b",
      "c",
      "d",
      "e",
      "f",
      "g",
      "h",
      "j",
    ]);
    expect(moved.bands).toEqual([]);
    expect(moved.computed).toEqual(initial.order);
    const restored = undoReview(JSON.parse(JSON.stringify(moved)));
    expect(restored.order).toEqual(initial.order);
    expect(restored.bands).toEqual(initial.bands);
    expect(restored.picks).toEqual(initial.picks);
    expect(restored.manual).toEqual([]);
    expect(restored.revision).toBe(3);
  });
  it("moves a multi-selection in ranking order even when clicked out of order", () => {
    const moved = moveReview(fixture(), ["i", "b"], 4);
    expect(moved.order).toEqual([
      "a",
      "c",
      "d",
      "b",
      "i",
      "e",
      "f",
      "g",
      "h",
      "j",
    ]);
    expect(new Set(moved.order).size).toBe(10);
  });
  it("splits an unmoved band when rank nine moves into its middle", () => {
    const initial = {
      ...fixture(),
      bands: [
        ["a", "b", "c"],
        ["f", "g"],
      ],
      manual: ["g"],
    };
    const before = JSON.stringify(initial);
    const moved = moveReview(initial, ["i"], 2);
    expect(moved.order).toEqual([
      "a",
      "i",
      "b",
      "c",
      "d",
      "e",
      "f",
      "g",
      "h",
      "j",
    ]);
    expect(moved.bands).toEqual([
      ["b", "c"],
      ["f", "g"],
    ]);
    expect(moved.picks).toEqual(initial.picks);
    expect(moved.computed).toEqual(initial.computed);
    expect(moved.manual).toEqual(["g", "i"]);
    expect(moved.history).toEqual([
      {
        order: initial.order,
        picks: initial.picks,
        bands: initial.bands,
        manual: initial.manual,
      },
    ]);
    expect(JSON.stringify(initial)).toBe(before);
  });
  it("retains two consecutive destination fragments without merging a neighboring band", () => {
    const initial = {
      ...fixture(),
      bands: [
        ["a", "b", "c", "d", "e", "f"],
        ["g", "h"],
      ],
    };
    const moved = moveReview(initial, ["i"], 4);
    expect(moved.order).toEqual([
      "a",
      "b",
      "c",
      "i",
      "d",
      "e",
      "f",
      "g",
      "h",
      "j",
    ]);
    expect(moved.bands).toEqual([
      ["a", "b", "c"],
      ["d", "e", "f"],
      ["g", "h"],
    ]);
    expect(moved.picks).toEqual(initial.picks);
  });
  it("splits both ends of a sparse move and serialized Undo restores the exact prior bands and history", () => {
    const initial = changeReview(
      {
        ...fixture(),
        bands: [["a", "b", "c", "d", "e", "f", "g", "h"]],
        manual: ["j"],
      },
      { picks: ["a", "f"] },
    );
    const before = JSON.stringify(initial);
    const moved = moveReview(initial, ["i", "d", "b", "d", "unknown"], 4);
    expect(moved.order).toEqual([
      "a",
      "c",
      "e",
      "b",
      "d",
      "i",
      "f",
      "g",
      "h",
      "j",
    ]);
    expect(moved.bands).toEqual([["f", "g", "h"]]);
    expect(moved.manual).toEqual(["j", "b", "d", "i"]);
    expect(moved.picks).toEqual(initial.picks);
    expect(moved.computed).toEqual(initial.computed);
    expect(moved.history).toHaveLength(initial.history.length + 1);
    const restored = undoReview(JSON.parse(JSON.stringify(moved)));
    expect(restored).toEqual({ ...initial, revision: moved.revision + 1 });
    expect(JSON.stringify(initial)).toBe(before);
  });
  it("keeps every band and history reference for a no-op inside a band", () => {
    const initial = {
      ...fixture(),
      bands: [["a", "b", "c", "d"]],
    };
    expect(moveReview(initial, ["c", "b"], 2)).toBe(initial);
  });
  it("keeps hidden entries in the full order and clamps targets", () => {
    const moved = moveReview(fixture(), ["b", "i"], 999);
    expect(moved.order).toEqual([
      "a",
      "c",
      "d",
      "e",
      "f",
      "g",
      "h",
      "j",
      "b",
      "i",
    ]);
    expect(moveReview(moved, ["b", "i"], -1).order.slice(0, 2)).toEqual([
      "b",
      "i",
    ]);
  });
  it("does not add history for a no-op", () =>
    expect(moveReview(fixture(), ["a"], 1).history).toHaveLength(0));
  it("uses the final block position rather than the pre-removal target position", () => {
    const initial = {
      ...fixture(),
      order: ["A", "B", "C", "D", "E"],
      bands: [],
    };
    expect(moveReview(initial, ["B", "D"], 3).order).toEqual([
      "A",
      "C",
      "B",
      "D",
      "E",
    ]);
  });
  it("keeps pick undo separate from image ordering", () => {
    const initial = fixture(),
      picked = changeReview(initial, { picks: ["a"] });
    expect(picked.order).toEqual(initial.order);
    expect(undoReview(picked).picks).toEqual(initial.picks);
  });
  it("recompares adjacent images only and makes both an explicit provisional band", () => {
    const initial = fixture(),
      compared = compareReview(initial, "b", "a", "left");
    expect(compared.order.slice(0, 2)).toEqual(["b", "a"]);
    expect(compareReview(compared, "b", "a", "both").bands).toContainEqual([
      "b",
      "a",
    ]);
    expect(() => compareReview(initial, "a", "c", "right")).toThrow("adjacent");
  });
  it("recompare splits the previous band and does not reconnect its remainder across the detached pair", () => {
    const initial = {
      ...fixture(),
      bands: [["a", "b", "c", "d", "e", "f", "g"]],
    };
    const compared = compareReview(initial, "c", "d", "both");
    expect(compared.bands).toEqual([
      ["a", "b"],
      ["e", "f", "g"],
      ["c", "d"],
    ]);
    expect(undoReview(compared).bands).toEqual(initial.bands);
  });
  it("Both across two bands creates exactly the selected pair without a transitive merge", () => {
    const initial = {
      ...fixture(),
      bands: [
        ["a", "b"],
        ["c", "d"],
      ],
    };
    expect(compareReview(initial, "b", "c", "both").bands).toEqual([
      ["b", "c"],
    ]);
  });
});
describe("visible range selection", () => {
  it("selects visible sorted ranges without changing hidden selections", () =>
    expect(rangeSelection(["hidden"], ["c", "a", "d", "b"], "a", "b")).toEqual([
      "hidden",
      "a",
      "d",
      "b",
    ]));
  it("deselects a range according to the clicked target state", () =>
    expect(
      rangeSelection(["hidden", "a", "b", "c"], ["a", "b", "c"], "a", "c"),
    ).toEqual(["hidden"]));
  it("treats a hidden anchor as a single toggle", () =>
    expect(rangeSelection(["hidden"], ["a", "b"], "hidden", "b")).toEqual([
      "hidden",
      "b",
    ]));
});
