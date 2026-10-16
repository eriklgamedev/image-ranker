// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import App, { validateSession } from "../../src/App";
import { demoReport } from "../../src/components/demo";
import {
  createRun,
  decide,
  getPair,
  getResult,
} from "../../src/domain/ranking";
import type { ImageItem } from "../../src/native";

vi.mock("../../src/native", () => ({
  desktop: false,
  previewUrl: (item: ImageItem) => item.previewPath,
  api: {},
}));
vi.mock("../../src/components/ImagePreview", () => ({
  ImagePreview: ({
    item,
    onReady,
  }: {
    item: ImageItem;
    onReady?: (ready: boolean) => void;
  }) => (
    <img
      alt={item.originalName}
      src={item.previewPath}
      onLoad={() => onReady?.(true)}
    />
  ),
}));
// A single mounted row proves that selection uses the complete filtered layout.
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: () => ({
    getTotalSize: () => 4000,
    getVirtualItems: () => [{ index: 0, key: "first-row", start: 0 }],
    measure: () => {},
  }),
}));
const key = "image-ranker-demo-session-v1";
const sources = [
  {
    id: "ink",
    path: "C:\\Fixtures\\Ink",
    name: "Ink",
    recursive: false,
    count: 12,
  },
  {
    id: "photos",
    path: "C:\\Fixtures\\Photos",
    name: "Photos",
    recursive: false,
    count: 4,
  },
];
function fixture() {
  const seed = demoReport().images[0];
  const images = Array.from({ length: 16 }, (_, index) => {
    const source = sources[index < 12 ? 0 : 1];
    const name = `${index % 3 === 0 ? "Sketch" : "Ink"}-${String(index).padStart(2, "0")}.jpg`;
    return {
      ...seed,
      id: `review-${index}`,
      path: `${source.path}\\${name}`,
      originalName: name,
      sourceIds: [source.id],
    };
  });
  let run = createRun(
    images.map((image) => image.id),
    "full-pairwise",
    17,
    false,
  );
  for (let pair = getPair(run); pair; pair = getPair(run))
    run = decide(run, pair.id, "left");
  const result = getResult(run);
  return validateSession({
    schemaVersion: 1,
    name: "Review selection fixture",
    report: { sources, images, ignored: [] },
    selected: images.map((image) => image.id),
    incomingSelected: true,
    run,
    review: {
      computed: result.order,
      order: result.order,
      bands: [],
      picks: [result.order.at(-1)!],
      manual: [result.order[5]],
      revision: 1,
      history: [],
    },
    archivedRuns: [],
    screen: "review",
    settings: {
      recursive: false,
      randomize: false,
      mode: "adaptive",
      cardSize: 172,
      showNames: true,
    },
  });
}
type Session = ReturnType<typeof fixture>;
beforeEach(() => {
  localStorage.clear();
  vi.stubGlobal("requestAnimationFrame", () => 1);
  vi.stubGlobal("cancelAnimationFrame", () => {});
  vi.stubGlobal(
    "PointerEvent",
    class extends MouseEvent {
      pointerId: number;
      pointerType: string;
      isPrimary: boolean;
      constructor(type: string, init: PointerEventInit = {}) {
        super(type, init);
        this.pointerId = init.pointerId ?? 1;
        this.pointerType = init.pointerType ?? "mouse";
        this.isPrimary = init.isPrimary ?? true;
      }
    },
  );
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(440);
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(300);
  vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockReturnValue(4000);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    function (this: HTMLElement) {
      if (this.classList.contains("grid-scroll"))
        return new DOMRect(0, 0, 440, 300);
      if (this.parentElement?.classList.contains("grid-scroll"))
        return new DOMRect(20, 20 - this.parentElement.scrollTop, 400, 4000);
      return new DOMRect();
    },
  );
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(private callback: ResizeObserverCallback) {}
      observe(target: Element) {
        this.callback(
          [
            {
              target,
              contentRect: new DOMRect(0, 0, 400, 300),
            } as ResizeObserverEntry,
          ],
          this as unknown as ResizeObserver,
        );
      }
      disconnect() {}
    },
  );
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
async function setup(value = fixture()) {
  localStorage.setItem(key, JSON.stringify(value));
  const view = render(<App />);
  await screen.findByText(
    value.screen === "arena"
      ? "Which image do you prefer?"
      : "Your ranking, refined.",
  );
  return {
    ...view,
    value,
    grid: view.container.querySelector<HTMLDivElement>(".grid-scroll")!,
  };
}
const stored = (): Session => JSON.parse(localStorage.getItem(key)!);
const selection = (rank = 1) =>
  screen.getByRole("button", { name: new RegExp(`^Select rank ${rank}:`) });
const summary = () =>
  screen.getByRole("status", { name: "Review summary" }).textContent;
const pointer = (x: number, y: number, extra = {}) => ({
  pointerId: 1,
  pointerType: "mouse",
  isPrimary: true,
  button: 0,
  buttons: 1,
  clientX: x,
  clientY: y,
  ...extra,
});
function start(target: Element, ctrlKey = false) {
  fireEvent.pointerDown(target, pointer(30, 30, { ctrlKey }));
  fireEvent.pointerMove(window, pointer(250, 150));
}
const finish = () =>
  fireEvent.pointerUp(window, pointer(250, 150, { buttons: 0 }));
function transfer(origin = "") {
  const data = new Map([["text/plain", origin]]);
  return {
    setData: (type: string, value: string) => data.set(type, value),
    getData: (type: string) => data.get(type) || "",
    effectAllowed: "",
    dropEffect: "",
  };
}

describe("Review rectangle selection and bulk final picks", () => {
  it("previews thumbnail drag selection, commits after release and suppresses the release click without a Review edit", async () => {
    const { container, value } = await setup();
    const first = selection();
    start(first);
    expect(container.querySelector(".selection-rectangle")).toBeTruthy();
    expect(first.getAttribute("aria-pressed")).toBe("true");
    expect(selection(2).getAttribute("aria-pressed")).toBe("true");
    expect(summary()).toContain("0 selected");
    finish();
    fireEvent.click(first, { detail: 1 });
    expect(summary()).toContain("2 selected");
    expect(container.querySelector(".selection-rectangle")).toBeNull();
    expect(stored().review).toEqual(value.review);
    expect(stored().run).toEqual(value.run);
  });
  it("Ctrl adds to a selection hidden by the folder filter and Escape cancels the next replacement", async () => {
    const { grid, container, value } = await setup();
    fireEvent.click(selection());
    fireEvent.change(screen.getByLabelText("Review source folder"), {
      target: { value: "photos" },
    });
    start(grid, true);
    finish();
    expect(summary()).toContain("3 selected");
    start(grid);
    expect(container.querySelector(".selection-rectangle")).toBeTruthy();
    fireEvent.keyDown(window, { key: "Escape" });
    finish();
    expect(summary()).toContain("3 selected");
    fireEvent.click(screen.getByRole("button", { name: "Star selected" }));
    await waitFor(() =>
      expect(new Set(stored().review!.picks)).toEqual(
        new Set([
          value.review!.order[0],
          ...value
            .review!.order.filter((id) =>
              value.report.images
                .find((image) => image.id === id)!
                .sourceIds.includes("photos"),
            )
            .slice(0, 2),
          ...value.review!.picks,
        ]),
      ),
    );
  });
  it("uses the filtered full layout to select offscreen images, then stars exactly that set", async () => {
    const { grid, value } = await setup();
    fireEvent.change(screen.getByLabelText("Review source folder"), {
      target: { value: "ink" },
    });
    fireEvent.change(screen.getByLabelText("Search ranked images"), {
      target: { value: "iNk" },
    });
    const expected = value.report.images
      .filter(
        (image) =>
          image.sourceIds.includes("ink") &&
          image.originalName.toLowerCase().includes("ink"),
      )
      .map((image) => image.id);
    expect(
      screen.queryByRole("button", { name: /^Select rank 12:/ }),
    ).toBeNull();
    start(grid);
    grid.scrollTop = 325 * 3;
    fireEvent.scroll(grid);
    finish();
    expect(summary()).toContain("8 selected");
    fireEvent.click(screen.getByRole("button", { name: "Star selected" }));
    await waitFor(() =>
      expect(new Set(stored().review!.picks)).toEqual(
        new Set([...value.review!.picks, ...expected]),
      ),
    );
    expect(stored().review!.order).toEqual(value.review!.order);
  });
  it("adds all selected images as one undoable saved edit, preserves existing picks and ranking details, and avoids no-op edits", async () => {
    const { value, unmount } = await setup();
    const star = screen.getByRole("button", { name: "Star selected" });
    expect(star.hasAttribute("disabled")).toBe(true);
    start(selection());
    finish();
    fireEvent.click(star);
    await waitFor(() => expect(stored().review!.history).toHaveLength(1));
    const changed = stored();
    expect(changed.review!.picks).toEqual([
      ...value.review!.picks,
      ...value.review!.order.slice(0, 2),
    ]);
    for (const field of ["order", "computed", "bands", "manual"] as const)
      expect(changed.review![field]).toEqual(value.review![field]);
    expect(changed.run).toEqual(value.run);
    expect(star.hasAttribute("disabled")).toBe(true);
    fireEvent.click(star);
    expect(stored().review!.history).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Review edit" }));
    await waitFor(() =>
      expect(stored().review!.picks).toEqual(value.review!.picks),
    );
    expect(summary()).toContain("2 selected");
    fireEvent.click(star);
    await waitFor(() =>
      expect(stored().review!.picks).toEqual(changed.review!.picks),
    );
    const saved = stored();
    unmount();
    render(<App />);
    await screen.findByText("Your ranking, refined.");
    expect(stored().review).toEqual(saved.review);
    expect(
      screen
        .getByRole("button", { name: "Remove final pick: Sketch-00.jpg" })
        .getAttribute("aria-pressed"),
    ).toBe("true");
  });
  it("keeps star, inspect, reorder and arrow controls separate from selection gestures", async () => {
    const { container } = await setup();
    const card = selection().closest("article")!;
    for (const name of [
      "Add final pick: Sketch-00.jpg",
      "Inspect rank 1",
      "Reorder rank 1. Use Alt plus arrow keys.",
      "Move rank 1 down",
    ]) {
      start(within(card).getByRole("button", { name }));
      finish();
      expect(container.querySelector(".selection-rectangle")).toBeNull();
      expect(summary()).toContain("0 selected");
    }
    fireEvent.click(
      within(card).getByRole("button", {
        name: "Add final pick: Sketch-00.jpg",
      }),
    );
    await waitFor(() => expect(stored().review!.picks).toContain("review-0"));
    expect(summary()).toContain("0 selected");
  });
  it("reserves native reorder dragging for the handle and accepts only the active local unfiltered drag", async () => {
    const { value } = await setup();
    const first = selection().closest("article")!;
    const second = selection(2).closest("article")!;
    const handle = within(first).getByRole("button", { name: /^Reorder rank/ });
    expect(first.draggable).toBe(false);
    expect(handle.draggable).toBe(true);
    const foreign = transfer("review-0");
    fireEvent.drop(second, { dataTransfer: foreign });
    expect(stored().review!.history).toHaveLength(0);
    const local = transfer();
    fireEvent.dragStart(handle, { dataTransfer: local });
    fireEvent.dragOver(second, { dataTransfer: local });
    fireEvent.drop(second, { dataTransfer: local });
    await waitFor(() =>
      expect(stored().review!.order.slice(0, 2)).toEqual([
        value.review!.order[1],
        value.review!.order[0],
      ]),
    );
    expect(stored().review!.picks).toEqual(value.review!.picks);
    fireEvent.change(screen.getByLabelText("Review source folder"), {
      target: { value: "ink" },
    });
    const filteredFirst = selection().closest("article")!;
    const filteredHandle = within(filteredFirst).getByRole("button", {
      name: /^Reorder rank/,
    });
    expect(filteredHandle.draggable).toBe(false);
    const before = stored().review;
    fireEvent.dragStart(filteredHandle, { dataTransfer: transfer() });
    fireEvent.drop(selection(2).closest("article")!, {
      dataTransfer: transfer("review-1"),
    });
    expect(stored().review).toEqual(before);
  });
  it("blocks selection, bulk stars and native reorder after a save error", async () => {
    const { container } = await setup();
    fireEvent.click(selection());
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("Fixture disk full");
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Add final pick: Sketch-00.jpg" }),
    );
    await screen.findByText("Save failed");
    expect(
      screen
        .getByRole("button", { name: "Star selected" })
        .hasAttribute("disabled"),
    ).toBe(true);
    start(selection(2));
    finish();
    fireEvent.click(selection(2));
    expect(summary()).toContain("1 selected");
    expect(container.querySelector(".selection-rectangle")).toBeNull();
    expect(
      screen.getByRole("button", {
        name: "Reorder rank 2. Use Alt plus arrow keys.",
      }).draggable,
    ).toBe(false);
  });
});

it("shows determinate Adaptive placement progress, advances it after a comparison and restores it on Undo", async () => {
  const value = fixture();
  value.run = createRun(value.selected, "adaptive", 17, false);
  value.review = null;
  value.screen = "arena";
  await setup(value);
  const bar = screen.getByRole("progressbar");
  expect(bar.getAttribute("value")).toBe("1");
  expect(bar.getAttribute("max")).toBe("16");
  expect(bar.getAttribute("aria-label")).toMatch(/placed|placement/i);
  expect(
    screen.getByRole("status", { name: "Comparison progress" }).textContent,
  ).toContain("1 of 16");
  for (const image of screen.getAllByRole("img")) fireEvent.load(image);
  fireEvent.click(screen.getByRole("button", { name: /Prefer left/ }));
  await waitFor(() => expect(bar.getAttribute("value")).toBe("2"));
  expect(
    screen.getByRole("status", { name: "Comparison progress" }).textContent,
  ).toContain("1 comparisons made");
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: /^Undo/ }).hasAttribute("disabled"),
    ).toBe(false),
  );
  fireEvent.click(screen.getByRole("button", { name: /^Undo/ }));
  await waitFor(() => expect(bar.getAttribute("value")).toBe("1"));
});
