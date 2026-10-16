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
import { ImagePreview } from "../../src/components/ImagePreview";
import { CacheSettings } from "../../src/components/CacheSettings";
import { boundPreview, prefetchPreviews } from "../../src/components/preview";
import { PREVIEW_DEADLINE_MS } from "../../src/components/previewScheduler";
import { Inspection } from "../../src/App";
import { demoReport } from "../../src/components/demo";
import type { PreviewResponse } from "../../src/native";
const mocks = vi.hoisted(() => ({
  rendererContext: vi.fn(),
  cancelImageWork: vi.fn(),
  requestPreview: vi.fn(),
  releasePreview: vi.fn(),
  cacheInfo: vi.fn(),
  clearCache: vi.fn(),
  setCacheLimit: vi.fn(),
}));
vi.mock("../../src/native", () => ({
  desktop: true,
  api: mocks,
  previewUrl: (item: { previewPath: string }) => item.previewPath,
}));
vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (value: string) => value,
}));
const images = demoReport().images;
const item = images[0];
function response(
  id: string,
  size: number,
  contentRevision: string,
  requestId: string,
): PreviewResponse {
  return {
    path: `/preview/${id}-${size}.png`,
    assetToken: `asset-${id}`,
    leaseId: `lease-${requestId}`,
    contentRevision,
    requestId,
    width: Math.min(size, 1024),
    height: 512,
    scale: 1,
    pipelineVersion: "v1",
    status: "ready",
  };
}
function deferRequests() {
  const pending: Array<{
    args: Parameters<typeof response>;
    resolve: (value: PreviewResponse) => void;
  }> = [];
  mocks.requestPreview.mockImplementation(
    (...args: Parameters<typeof response>) =>
      new Promise((resolve) => pending.push({ args, resolve })),
  );
  return pending;
}
beforeEach(() => {
  vi.resetAllMocks();
  mocks.rendererContext.mockResolvedValue("renderer-A");
  mocks.cancelImageWork.mockResolvedValue(1);
  mocks.releasePreview.mockResolvedValue(undefined);
  mocks.requestPreview.mockImplementation(
    async (...args: Parameters<typeof response>) => response(...args),
  );
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});
describe("bound preview cache leases", () => {
  it("shows a retryable timeout, targets only that request for cancellation and releases late pixels", async () => {
    vi.useFakeTimers();
    const pending = deferRequests(),
      ready = vi.fn();
    render(<ImagePreview item={item} onReady={ready} />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(pending).toHaveLength(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(PREVIEW_DEADLINE_MS);
    });
    expect(screen.getByRole("button", { name: "Retry preview" })).toBeTruthy();
    expect(
      screen.getByText(/exceeded 30 seconds, including waiting/),
    ).toBeTruthy();
    expect(mocks.cancelImageWork).toHaveBeenCalledExactlyOnceWith(
      pending[0].args[3],
    );
    await act(async () => pending[0].resolve(response(...pending[0].args)));
    expect(mocks.releasePreview).toHaveBeenCalledExactlyOnceWith(
      `lease-${pending[0].args[3]}`,
    );
    expect(screen.queryByRole("img")).toBeNull();
    expect(ready).not.toHaveBeenCalledWith(true);
    expect(vi.getTimerCount()).toBe(0);
  });
  it("eventually displays a healthy card after a temporary native capacity rejection", async () => {
    mocks.requestPreview.mockRejectedValueOnce(
      new Error("IMAGE_JOB_CAPACITY: wait for current image work"),
    );
    render(<ImagePreview item={item} />);
    await screen.findByRole("img");
    expect(mocks.requestPreview).toHaveBeenCalledTimes(2);
    expect(screen.queryByText("Preview unavailable")).toBeNull();
  });
  it("acquires thumbnail leases and keeps them until display unmounts", async () => {
    const ready = vi.fn();
    const view = render(<ImagePreview item={item} onReady={ready} />);
    expect(screen.queryByRole("img")).toBeNull();
    const image = await screen.findByRole("img");
    expect(mocks.requestPreview).toHaveBeenCalledWith(
      item.id,
      512,
      item.contentRevision,
      expect.any(String),
    );
    fireEvent.load(image);
    expect(ready).toHaveBeenLastCalledWith(true);
    expect(mocks.releasePreview).not.toHaveBeenCalled();
    view.unmount();
    await waitFor(() => expect(mocks.releasePreview).toHaveBeenCalledTimes(1));
  });
  it("releases a result that arrives after unmount without reporting readiness", async () => {
    const pending = deferRequests(),
      ready = vi.fn();
    const view = render(<ImagePreview item={item} onReady={ready} />);
    await waitFor(() => expect(pending).toHaveLength(1));
    view.unmount();
    await act(async () => pending[0].resolve(response(...pending[0].args)));
    expect(mocks.releasePreview).toHaveBeenCalledWith(
      `lease-${pending[0].args[3]}`,
    );
    expect(ready).not.toHaveBeenCalledWith(true);
  });
  it("releases stale responses and the previous visible lease when the content key changes", async () => {
    const pending = deferRequests(),
      ready = vi.fn();
    const view = render(<ImagePreview item={item} onReady={ready} />);
    await waitFor(() => expect(pending).toHaveLength(1));
    view.rerender(<ImagePreview item={images[1]} onReady={ready} />);
    await waitFor(() => expect(pending).toHaveLength(2));
    await act(async () => pending[0].resolve(response(...pending[0].args)));
    expect(screen.queryByRole("img")).toBeNull();
    expect(mocks.releasePreview).toHaveBeenCalledWith(
      `lease-${pending[0].args[3]}`,
    );
    await act(async () => pending[1].resolve(response(...pending[1].args)));
    fireEvent.load(screen.getByRole("img"));
    expect(ready).toHaveBeenLastCalledWith(true);
    view.rerender(
      <ImagePreview
        item={{ ...images[1], contentRevision: "new-revision" }}
        onReady={ready}
      />,
    );
    await waitFor(() =>
      expect(mocks.releasePreview).toHaveBeenCalledWith(
        `lease-${pending[1].args[3]}`,
      ),
    );
    expect(ready).toHaveBeenLastCalledWith(false);
    expect(screen.queryByRole("img")).toBeNull();
    await waitFor(() => expect(pending).toHaveLength(3));
    view.unmount();
    await act(async () => pending[2].resolve(response(...pending[2].args)));
    expect(mocks.releasePreview).toHaveBeenCalledTimes(3);
  });
  it.each([
    { requestId: "stale" },
    { contentRevision: "stale" },
    { width: Infinity },
    { assetToken: "" },
    { leaseId: "" },
  ])(
    "rejects malformed bound response %j and releases any returned valid lease",
    async (patch) => {
      mocks.requestPreview.mockImplementation(
        async (...args: Parameters<typeof response>) => ({
          ...response(...args),
          ...patch,
        }),
      );
      await expect(boundPreview(item, 512)).rejects.toThrow(
        "no longer matches",
      );
      expect(mocks.releasePreview).toHaveBeenCalledTimes(
        "leaseId" in patch ? 0 : 1,
      );
    },
  );
  it("returns an idempotent release callback", async () => {
    const preview = await boundPreview(item, 512);
    await Promise.all([
      preview.release(),
      preview.release(),
      preview.release(),
    ]);
    expect(mocks.releasePreview).toHaveBeenCalledTimes(1);
  });
  it("gives duplicate consumers independent leases and cancels only the unmounted request", async () => {
    const pending = deferRequests();
    const left = render(<ImagePreview item={item} />);
    const right = render(<ImagePreview item={item} />);
    await waitFor(() => expect(pending).toHaveLength(2));
    expect(pending[0].args[3]).not.toBe(pending[1].args[3]);
    left.unmount();
    expect(mocks.cancelImageWork).toHaveBeenCalledWith(pending[0].args[3]);
    await act(async () => {
      pending[0].resolve(response(...pending[0].args));
      pending[1].resolve(response(...pending[1].args));
    });
    expect(screen.getAllByRole("img")).toHaveLength(1);
    expect(mocks.releasePreview).toHaveBeenCalledTimes(1);
    right.unmount();
    await waitFor(() => expect(mocks.releasePreview).toHaveBeenCalledTimes(2));
  });
  it("drops queued old-context requests and releases every late old-context lease", async () => {
    const pending = deferRequests();
    const requests = images
      .slice(0, 5)
      .map((image) => boundPreview(image, 512).catch((error) => error.code));
    await waitFor(() => expect(pending).toHaveLength(4));
    mocks.rendererContext.mockResolvedValue("renderer-B");
    await act(async () =>
      pending.forEach((entry) => entry.resolve(response(...entry.args))),
    );
    expect(await Promise.all(requests)).toEqual(Array(5).fill("STALE_CONTEXT"));
    expect(mocks.requestPreview).toHaveBeenCalledTimes(4);
    expect(mocks.releasePreview).toHaveBeenCalledTimes(4);
  });
  it.each(["DECODE_BUSY", "IMAGE_WORK_BUSY"])(
    "offers direct retry after finite %s retries without refreshing the source",
    async (code) => {
      mocks.requestPreview.mockRejectedValue(
        new Error(`${code}: current worker is busy`),
      );
      render(<ImagePreview item={item} />);
      const retry = await screen.findByRole(
        "button",
        { name: "Retry preview" },
        { timeout: 2500 },
      );
      expect(mocks.requestPreview).toHaveBeenCalledTimes(4);
      mocks.requestPreview.mockImplementation(
        async (...args: Parameters<typeof response>) => response(...args),
      );
      fireEvent.click(retry);
      await screen.findByRole("img");
      expect(mocks.requestPreview).toHaveBeenCalledTimes(5);
    },
  );
  it("does not retry changed source bytes or offer a misleading retry control", async () => {
    mocks.requestPreview.mockRejectedValue(
      new Error("CONTENT_CHANGED: reconnect source"),
    );
    render(<ImagePreview item={{ ...item, availability: "changed" }} />);
    await screen.findByText("Preview unavailable");
    expect(mocks.requestPreview).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: "Retry preview" })).toBeNull();
  });
  it.each(["DECODE_CANCELLED", "IMAGE_JOB_CANCELLED"])(
    "offers manual retry after %s without immediately restarting canceled work",
    async (code) => {
      mocks.requestPreview.mockRejectedValue(new Error(code));
      render(<ImagePreview item={item} />);
      const retry = await screen.findByRole("button", {
        name: "Retry preview",
      });
      expect(mocks.requestPreview).toHaveBeenCalledTimes(1);
      mocks.requestPreview.mockImplementation(
        async (...args: Parameters<typeof response>) => response(...args),
      );
      fireEvent.click(retry);
      await screen.findByRole("img");
      expect(mocks.requestPreview).toHaveBeenCalledTimes(2);
    },
  );
  it("cancels a queued inspection without issuing native preview work", async () => {
    const pending = deferRequests();
    const busy = images.slice(0, 4).map((image) => boundPreview(image, 2048));
    await waitFor(() => expect(pending).toHaveLength(4));
    const view = render(<Inspection item={images[5]} onClose={() => {}} />);
    view.unmount();
    await act(async () =>
      pending.forEach((entry) => entry.resolve(response(...entry.args))),
    );
    const held = await Promise.all(busy);
    await Promise.all(held.map((preview) => preview.release()));
    expect(mocks.requestPreview).toHaveBeenCalledTimes(4);
  });
  it("releases a failed decode and ignores a later decode completion for those pixels", async () => {
    const ready = vi.fn();
    render(<ImagePreview item={item} onReady={ready} />);
    const image = await screen.findByRole("img");
    let resolve!: () => void;
    let reject!: (error: Error) => void;
    let index = 0;
    Object.defineProperty(image, "decode", {
      value: () =>
        new Promise<void>((yes, no) => {
          if (index++ === 0) resolve = yes;
          else reject = no;
        }),
    });
    fireEvent.load(image);
    fireEvent.load(image);
    await act(async () => reject(new Error("Bad pixels")));
    await screen.findByText("Preview unavailable");
    expect(mocks.releasePreview).toHaveBeenCalledTimes(1);
    await act(async () => resolve());
    expect(ready).not.toHaveBeenCalledWith(true);
  });
  it("keeps missing-image previews read-only while still leasing any displayed cached pixels", async () => {
    const ready = vi.fn();
    const view = render(
      <ImagePreview
        item={{ ...item, availability: "missing" }}
        onReady={ready}
      />,
    );
    fireEvent.load(await screen.findByRole("img"));
    expect(ready).not.toHaveBeenCalledWith(true);
    expect(mocks.releasePreview).not.toHaveBeenCalled();
    view.unmount();
    await waitFor(() => expect(mocks.releasePreview).toHaveBeenCalledTimes(1));
  });
  it("releases both completed and late prefetch responses and caps speculative opponents at two", async () => {
    const pending = deferRequests();
    const dispose = prefetchPreviews(images.slice(0, 5));
    await waitFor(() => expect(pending).toHaveLength(1));
    await act(async () => pending[0].resolve(response(...pending[0].args)));
    await waitFor(() => expect(pending).toHaveLength(2));
    expect(mocks.releasePreview).not.toHaveBeenCalled();
    dispose();
    dispose();
    await act(async () => pending[1].resolve(response(...pending[1].args)));
    expect(mocks.releasePreview).toHaveBeenCalledTimes(2);
  });
  it("releases inspection leases on size changes, unmount and late arrival", async () => {
    const pending = deferRequests();
    const view = render(<Inspection item={item} onClose={() => {}} />);
    await waitFor(() => expect(pending).toHaveLength(1));
    await act(async () => pending[0].resolve(response(...pending[0].args)));
    fireEvent.load(screen.getByRole("img"));
    expect(mocks.releasePreview).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: /Actual size/ }));
    await waitFor(() =>
      expect(mocks.releasePreview).toHaveBeenCalledWith(
        `lease-${pending[0].args[3]}`,
      ),
    );
    expect(screen.queryByRole("img")).toBeNull();
    expect(pending[1].args[1]).toBe(Math.max(item.width, item.height));
    view.unmount();
    await act(async () => pending[1].resolve(response(...pending[1].args)));
    expect(mocks.releasePreview).toHaveBeenCalledTimes(2);
  });
  it("reports pinned files retained after clear and explains usage above budget", async () => {
    const mb = 1048576;
    const before = {
      files: 6,
      sizeBytes: 300 * mb,
      budgetBytes: 128 * mb,
      pinnedFiles: 2,
      pinnedBytes: 240 * mb,
      maintenancePending: false,
      maintenanceRunning: false,
      inventoryKnown: true,
      pendingDeleteFiles: 0,
      pendingPreviews: 0,
      pixelReservedBytes: 0,
      retirementIssues: [],
      temporaryFiles: 0,
      temporaryBytes: 0,
      clearRequest: null,
    };
    mocks.cacheInfo.mockResolvedValue(before);
    mocks.clearCache.mockResolvedValue({
      ...before,
      files: 2,
      sizeBytes: 240 * mb,
      clearRequest: {
        requestId: "lease-test-clear",
        generation: "1",
        state: "completed",
        retainedFiles: 0,
        retainedBytes: 0,
        retainedCountsKnown: true,
        reasons: [],
        retryOf: null,
      },
    });
    render(<CacheSettings />);
    const clear = await screen.findByRole("button", {
      name: "Clear preview cache",
    });
    await waitFor(() => expect(clear.hasAttribute("disabled")).toBe(false));
    fireEvent.click(clear);
    await screen.findByText(
      /Unused cache entries cleared\. 2 files remain, including 2 displayed or prefetched previews/,
    );
    expect(screen.getByText(/240.0 MB in 2 files/)).toBeTruthy();
    expect(screen.getByText(/112.0 MB above the budget/)).toBeTruthy();
  });
});
