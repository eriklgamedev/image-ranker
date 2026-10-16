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
import userEvent from "@testing-library/user-event";
import App, { validateSession } from "../../src/App";
import { demoReport } from "../../src/components/demo";
import { basename } from "../../src/components/OperationDialog";
import {
  createRun,
  decide,
  getPair,
  getResult,
} from "../../src/domain/ranking";

// Only the persistence adapter is substituted. App, ImagePreview, VirtualGrid,
// and TanStack's virtualizer execute their production behavior in these tests.
vi.mock("../../src/native", () => ({
  desktop: false,
  previewUrl: (item: { previewPath: string }) => item.previewPath,
  api: {},
}));

const storageKey = "image-ranker-demo-session-v1";
type Session = ReturnType<typeof validateSession>;
type Screen = Session["screen"];

function fixture(screen: Screen): Session {
  const report = demoReport();
  report.images = report.images.slice(0, 6);
  report.sources[0].count = report.images.length;
  const selected = report.images.map((image) => image.id);
  let run = createRun(selected, "full-pairwise", 19, false);
  if (screen === "review") {
    for (let pair = getPair(run); pair; pair = getPair(run)) {
      run = decide(run, pair.id, "left");
    }
  }
  const result = screen === "review" ? getResult(run) : null;
  return validateSession({
    schemaVersion: 1,
    name: "Keyboard qualification",
    report,
    selected,
    incomingSelected: true,
    run: screen === "sources" ? null : run,
    review: result
      ? {
          computed: result.order,
          order: result.order,
          bands: result.bands.filter((band) => band.length > 1),
          picks: [result.order[4]],
          manual: [],
          revision: 1,
          history: [],
        }
      : null,
    archivedRuns: [],
    screen,
    settings: {
      recursive: false,
      randomize: false,
      mode: "full-pairwise",
      cardSize: 172,
      showNames: true,
    },
  });
}

function stored(): Session {
  return JSON.parse(localStorage.getItem(storageKey)!) as Session;
}

function imageName(value: Session, id: string) {
  return basename(value.report.images.find((image) => image.id === id)!.path);
}

function reviewCard(value: Session, id: string): HTMLElement {
  const selection = screen
    .getAllByRole("button", { name: /^Select rank / })
    .find((button) =>
      button.getAttribute("aria-label")?.endsWith(`: ${imageName(value, id)}`),
    );
  expect(selection, `Ranked card for ${id} must be mounted`).toBeTruthy();
  return selection!.closest("article")!;
}

function reorderHandle(value: Session, id: string): HTMLElement {
  return within(reviewCard(value, id)).getByRole("button", {
    name: /^Reorder rank /,
  });
}

beforeEach(() => {
  localStorage.clear();
  // JSDOM has no layout engine. Supply only viewport measurements: 400px with
  // 172px cards makes two columns. The real virtualizer chooses and keys rows.
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(400);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(700);
  vi.stubGlobal(
    "ResizeObserver",
    class {
      constructor(private callback: ResizeObserverCallback) {}
      observe(target: Element) {
        this.callback(
          [
            {
              target,
              contentRect: new DOMRect(0, 0, 400, 700),
              borderBoxSize: [{ inlineSize: 400, blockSize: 700 }],
              contentBoxSize: [{ inlineSize: 400, blockSize: 700 }],
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
  localStorage.clear();
});

describe("preview retry preserves the enclosing image action", () => {
  it.each(["sources", "arena", "review"] as const)(
    "%s exposes Retry as an independent native button without nested buttons",
    async (route) => {
      const value = fixture(route);
      localStorage.setItem(storageKey, JSON.stringify(value));
      const { container } = render(<App />);
      const id =
        route === "arena" ? getPair(value.run!)!.left : value.selected[0];
      fireEvent.error(
        await screen.findByRole("img", { name: imageName(value, id) }),
      );
      const retry = await screen.findByRole("button", {
        name: "Retry preview",
      });
      expect(retry.parentElement?.closest("button")).toBeNull();
      expect(container.querySelectorAll("button button")).toHaveLength(0);
    },
  );

  it.each([
    ["sources", "Space"],
    ["sources", "Enter"],
    ["sources", "pointer"],
    ["arena", "Space"],
    ["arena", "Enter"],
    ["arena", "pointer"],
    ["review", "Space"],
    ["review", "Enter"],
    ["review", "pointer"],
  ] as const)(
    "%s: %s retries failed pixels without changing selection, ranking, review or inspection",
    async (route, action) => {
      const value = fixture(route);
      localStorage.setItem(storageKey, JSON.stringify(value));
      render(<App />);
      const id =
        route === "arena" ? getPair(value.run!)!.left : value.selected[0];
      const name = imageName(value, id);
      fireEvent.error(await screen.findByRole("img", { name }));
      const retry = await screen.findByRole("button", {
        name: "Retry preview",
      });
      const user = userEvent.setup();
      retry.focus();
      if (action === "pointer") await user.click(retry);
      else await user.keyboard(action === "Space" ? "[Space]" : "[Enter]");
      await waitFor(() => {
        expect(stored().selected).toEqual(value.selected);
        expect(
          screen.queryByRole("button", { name: "Retry preview" }),
        ).toBeNull();
        expect(screen.getByRole("img", { name })).toBeTruthy();
      });
      expect(stored().run).toEqual(value.run);
      expect(stored().review).toEqual(value.review);
      expect(stored().archivedRuns).toEqual(value.archivedRuns);
      expect(screen.queryByRole("dialog")).toBeNull();
      if (route === "review") {
        expect(
          within(reviewCard(value, id))
            .getByRole("button", { name: /^Select rank / })
            .getAttribute("aria-pressed"),
        ).toBe("false");
      }
    },
  );
});

describe("keyboard Review moves across actual virtual rows", () => {
  it.each([
    { direction: "up", from: 2, to: 1, next: 0, key: "ArrowUp" },
    { direction: "down", from: 1, to: 2, next: 3, key: "ArrowDown" },
  ])(
    "keeps focus on the same image after a cross-row $direction move and accepts the next arrow",
    async ({ from, to, next, key }) => {
      const value = fixture("review");
      localStorage.setItem(storageKey, JSON.stringify(value));
      render(<App />);
      await screen.findByText("Your ranking, refined.");
      await screen.findByRole("button", {
        name: "Reorder rank 6. Use Alt plus arrow keys.",
      });
      const original = value.review!.order;
      const movedId = original[from];
      const originRow = reviewCard(value, movedId).closest(".virtual-row");
      const destinationRow = reviewCard(value, original[to]).closest(
        ".virtual-row",
      );
      expect(originRow).not.toBeNull();
      expect(destinationRow).not.toBeNull();
      expect(originRow).not.toBe(destinationRow);

      const user = userEvent.setup();
      reorderHandle(value, movedId).focus();
      await user.keyboard(`{Alt>}{${key}}{/Alt}`);
      const expected = [...original];
      expected.splice(from, 1);
      expected.splice(to, 0, movedId);
      await waitFor(() => expect(stored().review!.order).toEqual(expected));
      await waitFor(() => {
        expect(document.activeElement).toBe(reorderHandle(value, movedId));
      });

      // Do not refocus programmatically: the user's next command must still act
      // on that image, even though its old cell lived under another row parent.
      await user.keyboard(`{Alt>}{${key}}{/Alt}`);
      expected.splice(to, 1);
      expected.splice(next, 0, movedId);
      await waitFor(() => expect(stored().review!.order).toEqual(expected));
      expect(document.activeElement).toBe(reorderHandle(value, movedId));
      expect(stored().review!.picks).toEqual(value.review!.picks);
      expect(stored().review!.computed).toEqual(value.review!.computed);
      expect(stored().run).toEqual(value.run);
      expect(stored().review!.history).toHaveLength(2);
      expect(stored().review!.history[0].order).toEqual(original);
    },
  );

  it("does not reuse a settled keyboard focus request after a pointer move", async () => {
    const value = fixture("review");
    localStorage.setItem(storageKey, JSON.stringify(value));
    render(<App />);
    await screen.findByRole("button", {
      name: "Reorder rank 6. Use Alt plus arrow keys.",
    });
    const movedId = value.review!.order[2];
    const user = userEvent.setup();
    reorderHandle(value, movedId).focus();
    await user.keyboard("{Alt>}{ArrowUp}{/Alt}");
    await waitFor(() => expect(stored().review!.order[1]).toBe(movedId));
    expect(document.activeElement).toBe(reorderHandle(value, movedId));

    await user.click(
      within(reviewCard(value, movedId)).getByRole("button", {
        name: "Move rank 2 down",
      }),
    );
    await waitFor(() =>
      expect(stored().review!.order).toEqual(value.review!.order),
    );
    expect(document.activeElement).not.toBe(reorderHandle(value, movedId));
    expect(stored().review!.picks).toEqual(value.review!.picks);
  });
});

describe("concise accessible workflow announcements", () => {
  it("announces candidate selection totals without repeating filenames", async () => {
    const value = fixture("sources");
    localStorage.setItem(storageKey, JSON.stringify(value));
    render(<App />);
    const summary = await screen.findByRole("status", {
      name: "Selection summary",
    });
    expect(summary.getAttribute("aria-atomic")).toBe("true");
    expect(summary.textContent).toContain("6 selected");
    const user = userEvent.setup();
    await user.click(
      await screen.findByRole("checkbox", {
        name: `Select ${imageName(value, value.selected[0])}`,
      }),
    );
    await waitFor(() => expect(summary.textContent).toContain("5 selected"));
    expect(summary.textContent).not.toContain(".jpg");
  });

  it("announces Review selection and final-pick totals", async () => {
    const value = fixture("review");
    localStorage.setItem(storageKey, JSON.stringify(value));
    render(<App />);
    const summary = await screen.findByRole("status", {
      name: "Review summary",
    });
    await screen.findByRole("button", {
      name: "Reorder rank 6. Use Alt plus arrow keys.",
    });
    const id = value.review!.order[0];
    const user = userEvent.setup();
    await user.click(
      within(reviewCard(value, id)).getByRole("button", {
        name: /^Select rank /,
      }),
    );
    expect(summary.textContent).toContain("1 selected");
    await user.click(
      within(reviewCard(value, id)).getByRole("button", {
        name: `Add final pick: ${imageName(value, id)}`,
      }),
    );
    await waitFor(() => expect(summary.textContent).toContain("2 final picks"));
    expect(summary.getAttribute("aria-atomic")).toBe("true");
    expect(summary.textContent).not.toContain(".jpg");
  });

  it("names comparison progress and announces the updated count after a vote", async () => {
    const value = fixture("arena");
    localStorage.setItem(storageKey, JSON.stringify(value));
    render(<App />);
    const summary = await screen.findByRole("status", {
      name: "Comparison progress",
    });
    const progress = screen.getByRole("progressbar", {
      name: "Completed comparisons",
    });
    expect(progress.getAttribute("value")).toBe("0");
    for (const image of await screen.findAllByRole("img"))
      fireEvent.load(image);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: /Prefer left/ }));
    await waitFor(() =>
      expect(summary.textContent).toContain("1 comparisons made"),
    );
    expect(progress.getAttribute("value")).toBe("1");
    expect(summary.getAttribute("aria-atomic")).toBe("true");
    expect(summary.textContent).not.toContain(".jpg");
  });
});
