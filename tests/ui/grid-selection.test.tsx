// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { VirtualGrid } from "../../src/components/VirtualGrid";
import App, { validateSession } from "../../src/App";
import { demoReport } from "../../src/components/demo";

vi.mock("../../src/native", () => ({
  desktop: false,
  previewUrl: (item: { previewPath: string }) => item.previewPath,
  api: {},
}));

let frames: Map<number, FrameRequestCallback>, frameId: number;
beforeEach(() => {
  localStorage.clear();
  frames = new Map();
  frameId = 0;
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    frames.set(++frameId, callback);
    return frameId;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => frames.delete(id));
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
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(400);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(300);
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(440);
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(300);
  vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockImplementation(
    function (this: HTMLElement) {
      return (
        parseFloat(
          (this.firstElementChild as HTMLElement)?.style.height || "300",
        ) + 40
      );
    },
  );
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    function (this: HTMLElement) {
      if (this.classList.contains("grid-scroll"))
        return new DOMRect(0, 0, 440, 300);
      if (this.parentElement?.classList.contains("grid-scroll"))
        return new DOMRect(
          20,
          20 - this.parentElement.scrollTop,
          400,
          parseFloat(this.style.height),
        );
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
              borderBoxSize: [{ inlineSize: 400, blockSize: 300 }],
              contentBoxSize: [{ inlineSize: 400, blockSize: 300 }],
              devicePixelContentBoxSize: [],
            },
          ],
          this as unknown as ResizeObserver,
        );
      }
      unobserve() {}
      disconnect() {}
    },
  );
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

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
const ids = Array.from({ length: 60 }, (_, i) => `image-${i}`);
function setup(initial = [ids[5]], available = ids) {
  const commit = vi.fn(),
    click = vi.fn(),
    inspect = vi.fn();
  const props = {
    ids,
    size: 172,
    selection: {
      selected: initial,
      selectable: available,
      disabled: false,
      onCommit: commit,
    },
    render: (id: string, _index: number, selected?: boolean) => (
      <article
        data-preview={selected === undefined ? "none" : String(selected)}
      >
        <button
          data-grid-selection-surface
          onClick={click}
          onDoubleClick={inspect}
        >
          {id}
        </button>
        <label>
          <input type="checkbox" aria-label={`Check ${id}`} onChange={click} />
        </label>
        <button onClick={inspect}>Inspect {id}</button>
      </article>
    ),
  };
  const view = render(<VirtualGrid {...props} />);
  const grid = view.container.querySelector<HTMLDivElement>(".grid-scroll")!;
  const surface = () => screen.getByRole("button", { name: ids[0] });
  return { ...view, grid, surface, props, commit, click, inspect };
}
function dragStart(target: Element, ctrlKey = false) {
  fireEvent.pointerDown(target, pointer(30, 30, { ctrlKey }));
  fireEvent.pointerMove(window, pointer(250, 150));
}

describe("virtual grid rectangle selection", () => {
  it("previews a replacement then commits once; excludes unavailable IDs and suppresses the release click", () => {
    const v = setup(
      [ids[5]],
      ids.filter((id) => id !== ids[1]),
    );
    dragStart(v.surface());
    expect(v.commit).not.toHaveBeenCalled();
    expect(v.surface().closest("article")?.dataset.preview).toBe("true");
    expect(
      screen.getByRole("button", { name: ids[1] }).closest("article")?.dataset
        .preview,
    ).toBe("false");
    fireEvent.pointerUp(window, pointer(250, 150, { buttons: 0 }));
    fireEvent.click(v.surface(), { detail: 1 });
    expect(v.commit).toHaveBeenCalledExactlyOnceWith([ids[0]]);
    expect(v.click).not.toHaveBeenCalled();
    expect(v.container.querySelector(".selection-rectangle")).toBeNull();
  });
  it("Ctrl adds hit cards to the original selection, including a filtered-out selected image", () => {
    const v = setup(["hidden", ids[5]], [...ids, "hidden"]);
    dragStart(v.grid, true);
    fireEvent.pointerUp(window, pointer(250, 150, { buttons: 0 }));
    expect(v.commit).toHaveBeenCalledExactlyOnceWith([
      "hidden",
      ids[5],
      ids[0],
      ids[1],
    ]);
  });
  it("does not turn clicks, checkbox/control gestures, double-clicks or touch scrolling into rectangles", () => {
    const v = setup();
    fireEvent.pointerDown(v.surface(), pointer(30, 30));
    fireEvent.pointerMove(window, pointer(32, 32));
    fireEvent.pointerUp(window, pointer(32, 32, { buttons: 0 }));
    fireEvent.click(v.surface(), { detail: 1 });
    fireEvent.doubleClick(v.surface(), { detail: 2 });
    expect(v.click).toHaveBeenCalledTimes(1);
    expect(v.inspect).toHaveBeenCalledTimes(1);
    for (const target of [
      screen.getByRole("checkbox", { name: `Check ${ids[0]}` }),
      screen.getByRole("button", { name: `Inspect ${ids[0]}` }),
    ]) {
      dragStart(target);
      fireEvent.pointerUp(window, pointer(250, 150, { buttons: 0 }));
    }
    fireEvent.pointerDown(
      v.surface(),
      pointer(30, 30, { pointerType: "touch" }),
    );
    fireEvent.pointerMove(window, pointer(250, 150, { pointerType: "touch" }));
    fireEvent.pointerUp(
      window,
      pointer(250, 150, { pointerType: "touch", buttons: 0 }),
    );
    expect(v.commit).not.toHaveBeenCalled();
    expect(v.container.querySelector(".selection-rectangle")).toBeNull();
  });
  it.each(["escape", "pointercancel", "blur", "unmount"])(
    "%s cancels without changing the saved selection or retaining work",
    (reason) => {
      const v = setup();
      dragStart(v.surface());
      if (reason === "escape") fireEvent.keyDown(window, { key: "Escape" });
      else if (reason === "pointercancel")
        fireEvent.pointerCancel(window, pointer(250, 150));
      else if (reason === "blur") fireEvent(window, new Event("blur"));
      else v.unmount();
      fireEvent.pointerUp(window, pointer(250, 150, { buttons: 0 }));
      expect(v.commit).not.toHaveBeenCalled();
      expect(frames.size).toBe(0);
      expect(v.container.querySelector(".selection-rectangle")).toBeNull();
    },
  );
  it.each(["locked", "collection", "selection"])(
    "%s changes abort an in-flight gesture",
    (reason) => {
      const v = setup();
      dragStart(v.surface());
      v.rerender(
        <VirtualGrid
          {...v.props}
          ids={reason === "collection" ? ids.slice(1) : ids}
          selection={{
            ...v.props.selection,
            disabled: reason === "locked",
            selected:
              reason === "selection" ? [ids[6]] : v.props.selection.selected,
          }}
        />,
      );
      fireEvent.pointerUp(window, pointer(250, 150, { buttons: 0 }));
      expect(v.commit).not.toHaveBeenCalled();
      if (reason === "locked") {
        dragStart(v.grid);
        fireEvent.pointerUp(window, pointer(250, 150, { buttons: 0 }));
        expect(v.commit).not.toHaveBeenCalled();
      }
    },
  );
  it("selects traversed offscreen rows from geometry after scrolling and can edge-scroll", () => {
    const v = setup();
    expect(screen.queryByRole("button", { name: ids[30] })).toBeNull();
    dragStart(v.surface());
    v.grid.scrollTop = 271 * 15;
    fireEvent.scroll(v.grid);
    fireEvent.pointerMove(window, pointer(250, 150));
    fireEvent.pointerUp(window, pointer(250, 150, { buttons: 0 }));
    expect(v.commit).toHaveBeenCalledExactlyOnceWith(ids.slice(0, 32));
    v.grid.scrollTop = 0;
    fireEvent.scroll(v.grid);
    dragStart(v.grid);
    fireEvent.pointerMove(window, pointer(250, 295));
    act(() => {
      const queued = [...frames.values()];
      frames.clear();
      queued.forEach((callback) => callback(16));
    });
    expect(v.grid.scrollTop).toBeGreaterThan(0);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(v.commit).toHaveBeenCalledTimes(1);
  });
});

it("App persists one rectangle replacement, adds with Ctrl, and retains normal click and double-click inspection", async () => {
  const report = demoReport();
  report.images = report.images.slice(0, 6);
  report.sources[0].count = 6;
  const initial = validateSession({
    schemaVersion: 1,
    name: "Drag fixture",
    report,
    selected: report.images.map((image) => image.id),
    incomingSelected: true,
    run: null,
    review: null,
    archivedRuns: [],
    screen: "sources",
    settings: {
      recursive: false,
      randomize: false,
      mode: "adaptive",
      cardSize: 172,
      showNames: true,
    },
  });
  const key = "image-ranker-demo-session-v1";
  localStorage.setItem(key, JSON.stringify(initial));
  const view = render(<App />);
  await screen.findByRole("button", { name: "None" });
  const grid = view.container.querySelector<HTMLDivElement>(".grid-scroll")!;
  const cells = [...grid.querySelectorAll<HTMLElement>("[data-grid-id]")];
  const first = cells[0].querySelector("button")!;
  dragStart(first);
  fireEvent.pointerUp(window, pointer(250, 150, { buttons: 0 }));
  fireEvent.click(first, { detail: 1 });
  await waitFor(() =>
    expect(JSON.parse(localStorage.getItem(key)!).selected).toEqual(
      cells.slice(0, 2).map((cell) => cell.dataset.gridId),
    ),
  );
  fireEvent.pointerDown(grid, pointer(30, 292, { ctrlKey: true }));
  fireEvent.pointerMove(window, pointer(200, 320));
  fireEvent.pointerUp(window, pointer(200, 320, { buttons: 0 }));
  await waitFor(() =>
    expect(JSON.parse(localStorage.getItem(key)!).selected).toEqual(
      cells.slice(0, 3).map((cell) => cell.dataset.gridId),
    ),
  );
  fireEvent.pointerDown(first, pointer(30, 30));
  fireEvent.pointerUp(window, pointer(30, 30, { buttons: 0 }));
  fireEvent.click(first, { detail: 1 });
  await waitFor(() =>
    expect(JSON.parse(localStorage.getItem(key)!).selected).toEqual(
      cells.slice(1, 3).map((cell) => cell.dataset.gridId),
    ),
  );
  fireEvent.doubleClick(first, { detail: 2 });
  expect(await screen.findByRole("dialog")).toBeTruthy();
});
