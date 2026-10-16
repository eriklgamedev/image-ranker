// @vitest-environment jsdom
import { beforeEach, afterEach, describe, it, expect, vi } from "vitest";
import {
  render,
  screen,
  fireEvent,
  waitFor,
  cleanup,
} from "@testing-library/react";
import App, {
  validateSession,
  mergeScanBatch,
  mergeScanReport,
} from "../../src/App";
import { demoReport } from "../../src/components/demo";
import { createRun, getPair } from "../../src/domain/ranking";
vi.mock("../../src/native", () => ({
  desktop: false,
  previewUrl: (item: { previewPath: string }) => item.previewPath,
  api: {},
}));
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: () => ({
    getTotalSize: () => 700,
    getVirtualItems: () => [
      { index: 0, key: "row-0", start: 0 },
      { index: 1, key: "row-1", start: 260 },
      { index: 2, key: "row-2", start: 520 },
    ],
    measure: () => {},
  }),
}));
const key = "image-ranker-demo-session-v1";
function savedSession() {
  const report = demoReport();
  report.images = report.images.slice(0, 3);
  report.sources[0].count = 3;
  return {
    schemaVersion: 1,
    name: "Test collection",
    report,
    selected: report.images.map((i) => i.id),
    incomingSelected: true,
    run: createRun(
      report.images.map((i) => i.id),
      "full-pairwise",
      7,
      false,
    ),
    review: null,
    archivedRuns: [],
    screen: "arena",
    settings: {
      recursive: false,
      randomize: false,
      mode: "full-pairwise",
      cardSize: 172,
      showNames: true,
    },
  };
}
beforeEach(() => {
  localStorage.clear();
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});
describe("session validation", () => {
  it("rejects unrecognized versions without overwriting the stored document", async () => {
    localStorage.setItem(
      key,
      JSON.stringify({ ...savedSession(), schemaVersion: 55 }),
    );
    render(<App />);
    await screen.findByText("Your workspace needs recovery");
    expect(JSON.parse(localStorage.getItem(key)!).schemaVersion).toBe(55);
  });
  it("replays active and archived runs before accepting a session", () => {
    const value = savedSession();
    expect(validateSession(value).name).toBe("Test collection");
    const damaged = structuredClone(value);
    damaged.run.state.kind = "adaptive";
    expect(() => validateSession(damaged)).toThrow();
    const archive = {
      ...value,
      archivedRuns: [
        {
          run: { ...value.run, algorithmVersion: 22 },
          review: null,
          date: new Date().toISOString(),
        },
      ],
    };
    expect(() => validateSession(archive)).toThrow();
  });
  it("validates historical run references using retained images without displaying them in the active collection", () => {
    const current = validateSession(savedSession());
    const removed = mergeScanReport(current, {
      sources: [],
      images: [],
      ignored: [],
    });
    expect(removed.report.images).toEqual([]);
    expect(removed.report.retainedImages).toHaveLength(3);
    expect(removed.selected).toEqual([]);
    expect(validateSession(removed).run).toEqual(current.run);
    const lost = structuredClone(removed);
    lost.report.retainedImages = [];
    expect(() => validateSession(lost)).toThrow("missing catalog");
  });
});
describe("progressive scan arrivals", () => {
  it("select none applies to later arrivals while invert preserves the incoming default", () => {
    const current = validateSession(savedSession()),
      image = demoReport().images[3],
      event = {
        sourceId: "demo-source",
        scanned: 4,
        eligible: 4,
        batch: [image],
        done: false,
        cancelled: false,
      };
    const none = mergeScanBatch(
      { ...current, selected: [], incomingSelected: false },
      event,
      false,
    );
    expect(none.report.images).toHaveLength(4);
    expect(none.selected).toEqual([]);
    const inverted = mergeScanBatch(
      { ...current, selected: [], incomingSelected: true },
      event,
      false,
    );
    expect(inverted.selected).toEqual([image.id]);
  });
  it("merges duplicate batch IDs and keeps the existing run through partial cancellation", () => {
    const current = validateSession(savedSession()),
      event = {
        sourceId: "demo-source",
        scanned: 3,
        eligible: 3,
        batch: current.report.images,
        done: false,
        cancelled: false,
      };
    const merged = mergeScanBatch(current, event, false);
    expect(merged.report.images).toHaveLength(3);
    const cancelled = mergeScanReport(merged, {
      ...merged.report,
      cancelled: true,
    });
    expect(cancelled.run).toEqual(current.run);
    expect(cancelled.report.cancelled).toBe(true);
    expect(validateSession(cancelled)).toBeTruthy();
  });
});
describe("ranking persistence and interaction", () => {
  it("requires loaded images, prevents duplicate votes and restores exact pair on undo", async () => {
    const initial = savedSession();
    localStorage.setItem(key, JSON.stringify(initial));
    render(<App />);
    const left = await screen.findByRole("button", { name: /Prefer left/ });
    expect(left.hasAttribute("disabled")).toBe(true);
    for (const img of screen.getAllByRole("img")) fireEvent.load(img);
    await waitFor(() => expect(left.hasAttribute("disabled")).toBe(false));
    fireEvent.click(left);
    fireEvent.click(left);
    await waitFor(() =>
      expect(JSON.parse(localStorage.getItem(key)!).run.history.length).toBe(1),
    );
    fireEvent.click(screen.getByRole("button", { name: /^Undo/ }));
    await waitFor(() =>
      expect(JSON.parse(localStorage.getItem(key)!).run.history.length).toBe(0),
    );
    const restored = JSON.parse(localStorage.getItem(key)!);
    expect(getPair(restored.run)?.left).toBe(getPair(initial.run)?.left);
    expect(getPair(restored.run)?.right).toBe(getPair(initial.run)?.right);
  });
  it("keeps inspection and keyboard shortcuts separate from voting", async () => {
    localStorage.setItem(key, JSON.stringify(savedSession()));
    render(<App />);
    await screen.findByText("Which image do you prefer?");
    for (const img of screen.getAllByRole("img")) fireEvent.load(img);
    fireEvent.click(screen.getByRole("button", { name: "Inspect left image" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.keyDown(dialog, { key: "ArrowLeft" });
    expect(JSON.parse(localStorage.getItem(key)!).run.history.length).toBe(0);
    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });
  it("changing collection selection preserves the existing run", async () => {
    const initial = savedSession();
    localStorage.setItem(key, JSON.stringify(initial));
    render(<App />);
    await screen.findByText("Which image do you prefer?");
    fireEvent.click(screen.getByRole("button", { name: "Save & exit" }));
    await screen.findByRole("button", { name: "None" });
    fireEvent.click(screen.getByRole("button", { name: "None" }));
    await waitFor(() =>
      expect(JSON.parse(localStorage.getItem(key)!).selected).toEqual([]),
    );
    expect(JSON.parse(localStorage.getItem(key)!).run).toEqual(initial.run);
    expect(screen.getByRole("button", { name: /Resume ranking/ })).toBeTruthy();
  });
  it("blocks an old run when any candidate has changed bytes even if cached previews load", async () => {
    const initial = savedSession();
    localStorage.setItem(
      key,
      JSON.stringify({
        ...initial,
        report: {
          ...initial.report,
          images: initial.report.images.map((image, index) => ({
            ...image,
            availability: index === 2 ? "changed" : "ready",
          })),
        },
      }),
    );
    render(<App />);
    await screen.findByText("Which image do you prefer?");
    for (const img of screen.getAllByRole("img")) fireEvent.load(img);
    expect(
      screen
        .getByRole("button", { name: /Prefer left/ })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(screen.getByRole("alert").textContent).toContain(
      "missing, changed, or disconnected",
    );
    expect(JSON.parse(localStorage.getItem(key)!).run.history.length).toBe(0);
  });
  it("blocks further decisions on save failure and retries the identical in-memory state", async () => {
    localStorage.setItem(key, JSON.stringify(savedSession()));
    render(<App />);
    await screen.findByText("Which image do you prefer?");
    for (const img of screen.getAllByRole("img")) fireEvent.load(img);
    const storage = vi
      .spyOn(Storage.prototype, "setItem")
      .mockImplementation(() => {
        throw new Error("Disk quota reached");
      });
    fireEvent.click(screen.getByRole("button", { name: /Prefer left/ }));
    await screen.findByRole("button", { name: "Retry save" });
    expect(
      screen
        .getByRole("button", { name: /Prefer left/ })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(JSON.parse(localStorage.getItem(key)!).run.history.length).toBe(0);
    storage.mockRestore();
    fireEvent.click(screen.getByRole("button", { name: "Retry save" }));
    await waitFor(() =>
      expect(JSON.parse(localStorage.getItem(key)!).run.history.length).toBe(1),
    );
    expect(screen.queryByRole("button", { name: "Retry save" })).toBeNull();
  });
  it("supports one image directly in review and exposes copy-all without requiring a pick", async () => {
    const initial = savedSession();
    initial.report.images = initial.report.images.slice(0, 1);
    initial.selected = initial.selected.slice(0, 1);
    initial.run = createRun(initial.selected, "full-pairwise", 7, false);
    localStorage.setItem(key, JSON.stringify(initial));
    render(<App />);
    await screen.findByText("Your ranking, refined.");
    fireEvent.click(
      screen.getByRole("button", { name: "Create ranked copies" }),
    );
    await screen.findByRole("dialog", { name: "Create ranked copies" });
    expect(
      screen.getByText(
        "Create a ranked copy of every image in a chosen folder. Originals keep their current names and locations.",
      ),
    ).toBeTruthy();
    expect(screen.queryByText("Operation completed")).toBeNull();
  });
});

describe("persistent operation preferences", () => {
  it("preserves absent legacy defaults and rejects malformed optional fields", () => {
    const value = savedSession();
    const checked = validateSession(value);
    expect(checked.settings.manifestDefault).toBeUndefined();
    expect(checked.settings.verifyCopyDefault).toBeUndefined();
    expect(() =>
      validateSession({
        ...value,
        settings: { ...value.settings, manifestDefault: "yes" },
      }),
    ).toThrow();
    expect(() =>
      validateSession({
        ...value,
        settings: { ...value.settings, verifyCopyDefault: 1 },
      }),
    ).toThrow();
  });
  it("saves manifest and read-back defaults and restores them after relaunch", async () => {
    localStorage.setItem(key, JSON.stringify(savedSession()));
    const view = render(<App />);
    await screen.findByRole("button", { name: /Prefer left/ });
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const manifest = screen.getByLabelText(
      "Save JSON and CSV manifests by default",
    ) as HTMLInputElement;
    const readback = screen.getByLabelText(
      "Verify copied bytes by reading outputs back by default",
    ) as HTMLInputElement;
    expect(manifest.checked).toBe(true);
    expect(readback.checked).toBe(false);
    fireEvent.click(manifest);
    fireEvent.click(readback);
    await waitFor(() =>
      expect(JSON.parse(localStorage.getItem(key)!).settings).toMatchObject({
        manifestDefault: false,
        verifyCopyDefault: true,
      }),
    );
    view.unmount();
    render(<App />);
    await screen.findByRole("button", { name: /Prefer left/ });
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(
      (
        screen.getByLabelText(
          "Save JSON and CSV manifests by default",
        ) as HTMLInputElement
      ).checked,
    ).toBe(false);
    expect(
      (
        screen.getByLabelText(
          "Verify copied bytes by reading outputs back by default",
        ) as HTMLInputElement
      ).checked,
    ).toBe(true);
  });
});
