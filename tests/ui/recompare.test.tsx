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
import { RecompareDialog } from "../../src/components/RecompareDialog";
import { compareReview, type ReviewState } from "../../src/components/review";
import { demoReport } from "../../src/components/demo";
import type { ImageItem } from "../../src/native";
const mocks = vi.hoisted(() => ({
  rendererContext: vi.fn(),
  cancelImageWork: vi.fn(),
  requestPreview: vi.fn(),
  releasePreview: vi.fn(),
}));
vi.mock("../../src/native", () => ({
  desktop: true,
  api: mocks,
  previewUrl: (item: ImageItem) => item.previewPath,
}));
vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) => path,
}));
const images = demoReport().images;
const pair = images.slice(0, 2) as [ImageItem, ImageItem];
const response = (
  id: string,
  _size: number,
  contentRevision: string,
  requestId: string,
) => ({
  path: `/preview/${id}.png`,
  assetToken: `asset-${id}`,
  leaseId: `lease-${requestId}`,
  requestId,
  contentRevision,
  width: 2048,
  height: 1024,
  scale: 1,
  pipelineVersion: "v1",
  status: "ready",
});
beforeEach(() => {
  vi.resetAllMocks();
  mocks.rendererContext.mockResolvedValue("renderer-A");
  mocks.cancelImageWork.mockResolvedValue(1);
  mocks.releasePreview.mockResolvedValue(undefined);
  mocks.requestPreview.mockImplementation(
    async (...args: Parameters<typeof response>) => response(...args),
  );
});
afterEach(cleanup);
describe("verified review recompare", () => {
  it("waits for both native 2048px previews to load and decode before recording one decision", async () => {
    const decide = vi.fn();
    render(
      <RecompareDialog
        images={pair}
        eligible
        onDecide={decide}
        onClose={() => {}}
      />,
    );
    const controls = screen.getByRole("button", { name: "Both" });
    expect(controls.hasAttribute("disabled")).toBe(true);
    const previews = await screen.findAllByRole("img");
    const decode: Array<() => void> = [];
    for (const preview of previews) {
      Object.defineProperty(preview, "decode", {
        value: () => new Promise<void>((resolve) => decode.push(resolve)),
      });
      fireEvent.load(preview);
    }
    expect(controls.hasAttribute("disabled")).toBe(true);
    await act(async () => decode[0]());
    expect(controls.hasAttribute("disabled")).toBe(true);
    await act(async () => decode[1]());
    expect(controls.hasAttribute("disabled")).toBe(false);
    fireEvent.click(controls);
    fireEvent.click(controls);
    expect(decide).toHaveBeenCalledTimes(1);
    expect(decide).toHaveBeenCalledWith("both");
    expect(
      mocks.requestPreview.mock.calls.every(
        (call) =>
          call[1] === 2048 &&
          call[2] ===
            images.find((image) => image.id === call[0])?.contentRevision,
      ),
    ).toBe(true);
  });
  it("ignores old preview responses after the selected pair changes", async () => {
    const waiting: Array<{
      args: Parameters<typeof response>;
      resolve: (value: ReturnType<typeof response>) => void;
    }> = [];
    mocks.requestPreview.mockImplementation(
      (...args: Parameters<typeof response>) =>
        new Promise((resolve) => waiting.push({ args, resolve })),
    );
    const decide = vi.fn();
    const view = render(
      <RecompareDialog
        images={pair}
        eligible
        onDecide={decide}
        onClose={() => {}}
      />,
    );
    const nextPair = images.slice(2, 4) as [ImageItem, ImageItem];
    await waitFor(() => expect(waiting).toHaveLength(2));
    view.rerender(
      <RecompareDialog
        images={nextPair}
        eligible
        onDecide={decide}
        onClose={() => {}}
      />,
    );
    await waitFor(() => expect(waiting).toHaveLength(4));
    await act(async () => {
      waiting[1].resolve(response(...waiting[1].args));
      waiting[0].resolve(response(...waiting[0].args));
    });
    expect(screen.queryAllByRole("img")).toHaveLength(0);
    expect(
      screen
        .getByRole("button", { name: "Prefer left" })
        .hasAttribute("disabled"),
    ).toBe(true);
    await act(async () => {
      waiting[3].resolve(response(...waiting[3].args));
      waiting[2].resolve(response(...waiting[2].args));
    });
    for (const preview of screen.getAllByRole("img")) fireEvent.load(preview);
    await waitFor(() =>
      expect(
        screen
          .getByRole("button", { name: "Prefer left" })
          .hasAttribute("disabled"),
      ).toBe(false),
    );
    expect(
      screen
        .getAllByRole("img")
        .every((image) =>
          nextPair.some((item) => image.getAttribute("src")?.includes(item.id)),
        ),
    ).toBe(true);
  });
  it("rejects a mismatched content revision even when its image URL would load", async () => {
    mocks.requestPreview.mockImplementation(
      async (...args: Parameters<typeof response>) => ({
        ...response(...args),
        contentRevision: "stale",
      }),
    );
    render(
      <RecompareDialog
        images={pair}
        eligible
        onDecide={() => {}}
        onClose={() => {}}
      />,
    );
    await screen.findAllByText("Preview unavailable");
    expect(
      screen.getByRole("button", { name: "Both" }).hasAttribute("disabled"),
    ).toBe(true);
  });
  it("disables judgments when an image or its source grant is unavailable", async () => {
    const decide = vi.fn();
    render(
      <RecompareDialog
        images={pair}
        eligible={false}
        onDecide={decide}
        onClose={() => {}}
      />,
    );
    for (const preview of await screen.findAllByRole("img"))
      fireEvent.load(preview);
    fireEvent.click(screen.getByRole("button", { name: "Prefer right" }));
    expect(decide).not.toHaveBeenCalled();
    expect(
      screen.getByText(
        "Reconnect unchanged source images before making another comparison.",
      ),
    ).toBeTruthy();
  });
  it("preserves final picks and records a Both band in global review order", async () => {
    const ids = pair.map((image) => image.id);
    let review: ReviewState = {
      computed: ids,
      order: ids,
      picks: [ids[0]],
      bands: [],
      manual: [],
      revision: 1,
      history: [],
    };
    render(
      <RecompareDialog
        images={[pair[1], pair[0]]}
        eligible
        onDecide={(choice) => {
          review = compareReview(review, ids[1], ids[0], choice);
        }}
        onClose={() => {}}
      />,
    );
    for (const preview of await screen.findAllByRole("img"))
      fireEvent.load(preview);
    fireEvent.click(screen.getByRole("button", { name: "Both" }));
    expect(review.picks).toEqual([ids[0]]);
    expect(review.order).toEqual(ids);
    expect(review.bands).toEqual([ids]);
    expect(review.history).toHaveLength(1);
  });
});
