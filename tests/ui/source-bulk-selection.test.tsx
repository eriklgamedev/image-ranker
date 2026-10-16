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
import App, { validateSession } from "../../src/App";
import { demoReport } from "../../src/components/demo";
import { createRun } from "../../src/domain/ranking";
import type { ImageItem, ScanProgress, ScanReport } from "../../src/native";

const native = vi.hoisted(() => ({
  desktop: false,
  getBootStatus: vi.fn(),
  loadSessionState: vi.fn(),
  rendererContext: vi.fn(),
  recoveryWorkStatus: vi.fn(),
  recoveryNotes: vi.fn(),
  listOperations: vi.fn(),
  subscribeCloseRequested: vi.fn(),
  saveSession: vi.fn(),
  subscribeScanProgress: vi.fn(),
  scanSources: vi.fn(),
}));
vi.mock("../../src/native", () => ({
  get desktop() {
    return native.desktop;
  },
  previewUrl: (item: ImageItem) => item.previewPath,
  api: native,
}));
vi.mock("../../src/components/ImagePreview", () => ({
  ImagePreview: ({ item }: { item: ImageItem }) => (
    <img alt={item.originalName} src={item.previewPath} />
  ),
}));
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: () => ({
    getTotalSize: () => 700,
    getVirtualItems: () => [{ index: 0, key: "only-mounted-row", start: 0 }],
    measure: () => {},
  }),
}));
const key = "image-ranker-demo-session-v1";
const sourceA = {
  id: "source-a",
  path: "C:\\Fixtures\\Ink",
  name: "Ink",
  recursive: false,
  count: 18,
};
const sourceB = {
  id: "source-b",
  path: "C:\\Fixtures\\Photos",
  name: "Photos",
  recursive: false,
  count: 4,
};
const seed = demoReport().images[0];
function image(id: string, source = sourceA, name = `${id}.jpg`): ImageItem {
  return {
    ...seed,
    id,
    originalName: name,
    path: `${source.path}\\${name}`,
    sourceIds: [source.id],
  };
}
const imagesA = Array.from({ length: 16 }, (_, index) =>
  image(
    `a-${index}`,
    sourceA,
    `${index % 3 === 0 ? "Ink Hero" : "Sketch"} ${String(index).padStart(2, "0")}.JPG`,
  ),
);
const imagesB = Array.from({ length: 4 }, (_, index) =>
  image(`b-${index}`, sourceB, `Ink Hero ${index}.jpg`),
);
const unavailable = [
  { ...image("a-missing"), availability: "missing" as const },
  { ...image("a-changed"), availability: "changed" as const },
];
function fixture() {
  return validateSession({
    schemaVersion: 1,
    name: "Bulk fixture",
    report: {
      sources: [sourceA, sourceB],
      images: [...imagesA, ...unavailable, ...imagesB],
      ignored: [],
    },
    selected: [imagesA[0].id, imagesB[0].id],
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
}
type Session = ReturnType<typeof fixture>;
let nativeSession: Session;
beforeEach(() => {
  vi.resetAllMocks();
  native.desktop = false;
  localStorage.clear();
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  nativeSession = fixture();
  native.getBootStatus.mockResolvedValue({
    mode: "ready",
    state: "ready",
    issues: [],
    contextId: "context-test",
    profileGeneration: "profile-test",
  });
  native.loadSessionState.mockImplementation(async () => ({
    session: structuredClone(nativeSession),
    saveStatus: { state: "saved", durableRevision: 1 },
    contextId: "context-test",
    profileGeneration: "profile-test",
    sessionGeneration: "session-test",
  }));
  native.rendererContext.mockResolvedValue("context-test");
  native.recoveryWorkStatus.mockResolvedValue({
    active: null,
    recent: [],
    limit: 1,
  });
  native.recoveryNotes.mockResolvedValue([]);
  native.listOperations.mockResolvedValue([]);
  native.subscribeCloseRequested.mockResolvedValue(() => {});
  native.saveSession.mockImplementation(async (value: Session) => {
    nativeSession = structuredClone(value);
  });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});
async function setup(value = fixture(), desktop = false) {
  native.desktop = desktop;
  nativeSession = value;
  localStorage.setItem(key, JSON.stringify(value));
  const view = render(<App />);
  await screen.findByRole("button", { name: "None" });
  return view;
}
function stored(): Session {
  return native.desktop
    ? nativeSession
    : JSON.parse(localStorage.getItem(key)!);
}
function folder(source = sourceA) {
  fireEvent.click(screen.getByTitle(source.path));
}
function search(value: string) {
  fireEvent.change(screen.getByLabelText("Search filenames"), {
    target: { value },
  });
}
async function bulk(name: "All" | "None" | "Invert", expected: string[]) {
  fireEvent.click(screen.getByRole("button", { name }));
  await waitFor(() =>
    expect(new Set(stored().selected)).toEqual(new Set(expected)),
  );
}
const heroes = imagesA.filter((item) =>
  item.originalName.toLowerCase().includes("hero"),
);

describe("source bulk actions replace selection with the current view", () => {
  it("All selects every eligible image in the folder, including unmounted rows, and clears other folders", async () => {
    await setup();
    folder();
    expect(
      screen.queryByRole("checkbox", {
        name: `Select ${imagesA[14].originalName}`,
      }),
    ).toBeNull();
    await bulk(
      "All",
      imagesA.map((item) => item.id),
    );
    expect(stored().incomingSelected).toBe(false);
  });
  it("None clears the overall selection while preserving the active ranking", async () => {
    const value = fixture();
    value.run = createRun(
      imagesB.map((item) => item.id),
      "adaptive",
      7,
      false,
    );
    await setup(value);
    folder();
    await bulk("None", []);
    expect(stored().run).toEqual(value.run);
  });
  it("Invert selects only previously unselected eligible images in this folder, clearing hidden selections", async () => {
    await setup();
    folder();
    await bulk(
      "Invert",
      imagesA.slice(1).map((item) => item.id),
    );
    expect(stored().incomingSelected).toBe(true);
  });
  it("combines the folder and case-insensitive filename search, and treats an empty view as empty", async () => {
    await setup();
    folder();
    search("hErO");
    await bulk(
      "All",
      heroes.map((item) => item.id),
    );
    search("no matching filename");
    await bulk("All", []);
    await bulk("Invert", []);
  });
  it("unfiltered All images covers the whole eligible collection and keeps the incoming default", async () => {
    await setup();
    await bulk(
      "All",
      [...imagesA, ...imagesB].map((item) => item.id),
    );
    expect(stored().incomingSelected).toBe(true);
  });
  it("uses filename search across All images, reports the eligible scope, and keeps empty-view controls usable", async () => {
    await setup();
    search("HERO");
    expect(screen.getByText("Current view · 10 matching")).toBeTruthy();
    expect(screen.getByRole("button", { name: "All" }).title).toContain(
      "Selections outside this view are cleared.",
    );
    await bulk(
      "All",
      [...heroes, ...imagesB].map((item) => item.id),
    );
    expect(stored().incomingSelected).toBe(false);
    search("does not exist");
    expect(screen.getByText("Current view · 0 matching")).toBeTruthy();
    expect(
      screen.getByRole("button", { name: "None" }).hasAttribute("disabled"),
    ).toBe(false);
    await bulk("None", []);
  });
  it("persists the folder result after reload and starts a ranking with exactly those candidates", async () => {
    const value = fixture();
    value.run = createRun(
      imagesB.map((item) => item.id),
      "adaptive",
      9,
      false,
    );
    const first = await setup(value);
    folder();
    search("HERO");
    await bulk(
      "All",
      heroes.map((item) => item.id),
    );
    first.unmount();
    render(<App />);
    await screen.findByRole("button", { name: "Rank 6 images" });
    expect(new Set(stored().selected)).toEqual(
      new Set(heroes.map((item) => item.id)),
    );
    fireEvent.click(screen.getByRole("button", { name: "Rank 6 images" }));
    fireEvent.click(
      await screen.findByRole("button", { name: "Start comparing" }),
    );
    await waitFor(() =>
      expect(new Set(stored().run!.candidates)).toEqual(
        new Set(heroes.map((item) => item.id)),
      ),
    );
    expect(stored().archivedRuns[0].run).toEqual(value.run);
  });
});

describe("bulk selection during a native progressive scan", () => {
  async function scanFixture() {
    let progress!: (event: ScanProgress) => void;
    let finish!: (report: ScanReport) => void;
    native.subscribeScanProgress.mockImplementation(async (handler) => {
      progress = handler;
      return () => {};
    });
    native.scanSources.mockImplementation(
      () =>
        new Promise<ScanReport>((resolve) => {
          finish = resolve;
        }),
    );
    await setup(fixture(), true);
    folder();
    search("HERO");
    fireEvent.click(screen.getByRole("button", { name: "Add folder" }));
    await waitFor(() => expect(native.scanSources).toHaveBeenCalled());
    const incoming = [
      image("new-hero", sourceA, "Hero new.jpg"),
      image("new-sketch", sourceA, "Sketch new.jpg"),
      image("other-hero", sourceB, "Hero other.jpg"),
    ];
    const emit = async (batch: ImageItem[]) => {
      await act(async () =>
        progress({
          sourceId: batch[0].sourceIds[0],
          scanned: batch.length,
          eligible: batch.length,
          batch,
          done: false,
          cancelled: false,
        }),
      );
    };
    const end = async (additions: ImageItem[], cancelled = false) => {
      await act(async () =>
        finish({
          ...fixture().report,
          images: [...fixture().report.images, ...additions],
          cancelled,
        }),
      );
      await waitFor(() =>
        expect(screen.queryByText("Scanning folders…")).toBeNull(),
      );
    };
    return { incoming, emit, end };
  }
  it.each(["All", "None", "Invert"] as const)(
    "%s constrains late arrivals to the captured view and survives partial scan cancellation",
    async (action) => {
      const s = await scanFixture();
      fireEvent.click(screen.getByRole("button", { name: action }));
      await waitFor(() => expect(native.saveSession).toHaveBeenCalled());
      folder(sourceB);
      search("");
      await s.emit(s.incoming.slice(0, 2));
      await s.emit(s.incoming.slice(2));
      const base =
        action === "All" ? heroes : action === "Invert" ? heroes.slice(1) : [];
      const expected = [
        ...base.map((item) => item.id),
        ...(action === "None" ? [] : [s.incoming[0].id]),
      ];
      await s.end(s.incoming, true);
      await waitFor(() =>
        expect(new Set(stored().selected)).toEqual(new Set(expected)),
      );
      expect(stored().report.cancelled).toBe(true);
    },
  );
  it("Invert keeps a preceding captured incoming policy while explicit decisions override already arrived images", async () => {
    const s = await scanFixture();
    fireEvent.click(screen.getByRole("button", { name: "All" }));
    await s.emit(s.incoming.slice(0, 1));
    fireEvent.click(screen.getByRole("button", { name: "Invert" }));
    const later = image("later-hero", sourceA, "Hero later.jpg");
    await s.emit([later]);
    await s.emit(s.incoming.slice(1));
    await s.end([...s.incoming, later]);
    await waitFor(() => expect(stored().selected).toEqual([later.id]));
    expect(stored().incomingSelected).toBe(false);
  });
  it("keeps a per-image scan decision after filtered All and resets its captured rule for a later import", async () => {
    const s = await scanFixture();
    fireEvent.click(screen.getByRole("button", { name: "All" }));
    await s.emit(s.incoming.slice(0, 1));
    fireEvent.click(
      screen.getByRole("checkbox", { name: "Select Hero new.jpg" }),
    );
    await s.end(s.incoming.slice(0, 1));
    await waitFor(() =>
      expect(new Set(stored().selected)).toEqual(
        new Set(heroes.map((item) => item.id)),
      ),
    );
    const before = stored();
    fireEvent.click(screen.getByRole("button", { name: "Add folder" }));
    await waitFor(() => expect(native.scanSources).toHaveBeenCalledTimes(2));
    const later = image("next-import-hero", sourceA, "Hero independent.jpg");
    await s.emit([later]);
    await s.end([...s.incoming.slice(0, 1), later]);
    expect(stored().selected).toEqual(before.selected);
    expect(stored().selected).not.toContain(later.id);
  });
});
