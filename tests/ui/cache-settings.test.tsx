// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { CacheSettings } from "../../src/components/CacheSettings";
import type { CacheClearResult, CacheInfo } from "../../src/native";

const mocks = vi.hoisted(() => ({
  cacheInfo: vi.fn(),
  clearCache: vi.fn(),
  setCacheLimit: vi.fn(),
}));
vi.mock("../../src/native", () => ({ desktop: true, api: mocks }));
const mb = 1048576;
const snapshot = (changes: Partial<CacheInfo> = {}): CacheInfo => ({
  sizeBytes: 300 * mb,
  files: 6,
  budgetBytes: 128 * mb,
  pinnedBytes: 240 * mb,
  pinnedFiles: 2,
  pendingDeleteFiles: 0,
  pendingPreviews: 0,
  pixelReservedBytes: 0,
  retirementIssues: [],
  temporaryFiles: 0,
  temporaryBytes: 0,
  maintenancePending: false,
  maintenanceRunning: false,
  inventoryKnown: true,
  clearRequest: null,
  ...changes,
});
const receipt = (
  changes: Partial<CacheClearResult> = {},
): CacheClearResult => ({
  requestId: "clear-request-A",
  generation: "7",
  state: "pending",
  retainedFiles: 0,
  retainedBytes: 0,
  retainedCountsKnown: false,
  reasons: [],
  retryOf: null,
  ...changes,
});
const complete = () =>
  receipt({ state: "completed", retainedCountsKnown: true });
function deferred<T>() {
  let resolve!: (value: T) => void, reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
async function mount() {
  await act(async () => {
    render(<CacheSettings />);
  });
}
async function tick(milliseconds = 1000) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(milliseconds);
  });
}
beforeEach(() => {
  vi.useFakeTimers();
  vi.resetAllMocks();
  mocks.cacheInfo.mockResolvedValue(snapshot());
  mocks.clearCache.mockResolvedValue(
    snapshot({ maintenancePending: true, clearRequest: receipt() }),
  );
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("cache maintenance status", () => {
  it.each([
    [
      "retained retirement warning",
      "failed",
      "CACHE_RETIREMENT_RETRY: 4 unused entries remain",
    ],
    [
      "cancelled clear",
      "cancelled",
      "CACHE_CANCELLED: cleanup stopped at a safe boundary",
    ],
    [
      "error after dispatcher owner removal",
      "failed",
      "CACHE_OWNER_REMOVED: request ended with retained entries",
    ],
  ] as const)(
    "does not infer clear success from idle inventory after %s",
    async (_name, state, reason) => {
      const terminal = {
        ...snapshot(),
        clearRequest: receipt({
          state,
          retainedCountsKnown: true,
          retainedFiles: 4,
          retainedBytes: 60 * mb,
          reasons: [reason],
        }),
      };
      mocks.clearCache.mockResolvedValue(terminal);
      await mount();
      await act(async () => {
        fireEvent.click(
          screen.getByRole("button", { name: "Clear preview cache" }),
        );
      });
      expect(screen.queryByText(/Unused cache entries cleared/)).toBeNull();
      expect(screen.getByText(reason)).toBeTruthy();
      expect(
        screen.getByText(/2 displayed or prefetched previews/),
      ).toBeTruthy();
    },
  );
  it("does not claim a deferred clear completed before inventory settles", async () => {
    await mount();
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Clear preview cache" }),
      );
    });
    expect(screen.queryByText(/Unused cache entries cleared/)).toBeNull();
    expect(screen.getByRole("status").textContent).toMatch(/queued|pending/i);
    expect(screen.getByText(/2 displayed or prefetched previews/)).toBeTruthy();
    mocks.cacheInfo.mockResolvedValue(
      snapshot({ files: 2, sizeBytes: 240 * mb, clearRequest: complete() }),
    );
    await tick();
    expect(screen.getByRole("status").textContent).toContain(
      "Unused cache entries cleared",
    );
    expect(screen.getByRole("status").textContent).toContain("2 files remain");
    await tick(5000);
    expect(mocks.cacheInfo).toHaveBeenCalledTimes(2);
    expect(mocks.cacheInfo.mock.calls).toEqual([
      [true],
      [false, "clear-request-A"],
    ]);
  });

  it.each([
    ["expired receipt", null],
    [
      "different request",
      receipt({ ...complete(), requestId: "clear-request-B" }),
    ],
    [
      "different generation",
      receipt({ ...complete(), generation: "18446744073709551615" }),
    ],
  ] as const)(
    "keeps an idle inventory unconfirmed for an %s",
    async (_name, clearRequest) => {
      await mount();
      await act(async () => {
        fireEvent.click(
          screen.getByRole("button", { name: "Clear preview cache" }),
        );
      });
      mocks.cacheInfo.mockResolvedValue(snapshot({ clearRequest }));
      await tick();
      expect(mocks.cacheInfo).toHaveBeenLastCalledWith(
        false,
        "clear-request-A",
      );
      expect(screen.getByRole("status").textContent).toContain(
        "outcome is unconfirmed",
      );
      expect(screen.queryByText(/Unused cache entries cleared/)).toBeNull();
      await tick();
      expect(mocks.cacheInfo).toHaveBeenCalledTimes(3);
      mocks.cacheInfo.mockResolvedValue(snapshot({ clearRequest: complete() }));
      await tick();
      expect(screen.getByRole("status").textContent).toContain(
        "Unused cache entries cleared",
      );
    },
  );

  it("recovers a failed native request on remount and preserves its failure across a budget edit", async () => {
    const failed = receipt({
      state: "failed",
      retainedFiles: 4,
      retainedBytes: 60 * mb,
      retainedCountsKnown: true,
      reasons: ["CACHE_DELETE_RETRY: retained source remains"],
    });
    mocks.cacheInfo.mockResolvedValue(snapshot({ clearRequest: failed }));
    await mount();
    expect(screen.getByRole("status").textContent).toContain(
      "Clear request failed",
    );
    expect(screen.getByText(/4 retained cache files/)).toBeTruthy();
    mocks.setCacheLimit.mockResolvedValue(
      snapshot({ budgetBytes: 512 * mb, clearRequest: failed }),
    );
    await act(async () => {
      fireEvent.change(screen.getByLabelText("Cache budget"), {
        target: { value: String(512 * mb) },
      });
    });
    expect(screen.getByRole("status").textContent).toContain(
      "Clear request failed",
    );
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Refresh cache status" }),
      );
    });
    expect(mocks.cacheInfo).toHaveBeenLastCalledWith(true, "clear-request-A");
    expect(
      screen.getByText("CACHE_DELETE_RETRY: retained source remains"),
    ).toBeTruthy();
    expect(mocks.clearCache).not.toHaveBeenCalled();
  });

  it("preserves immutable terminal failure details if a later status incorrectly reports success", async () => {
    const failed = receipt({
      state: "failed",
      reasons: ["Original deletion failure"],
    });
    mocks.cacheInfo
      .mockResolvedValueOnce(snapshot({ clearRequest: failed }))
      .mockResolvedValue(snapshot({ clearRequest: complete() }));
    await mount();
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Refresh cache status" }),
      );
    });
    expect(screen.getByText("Original deletion failure")).toBeTruthy();
    expect(screen.getByRole("status").textContent).toContain(
      "outcome is unconfirmed",
    );
    expect(screen.queryByText(/Unused cache entries cleared/)).toBeNull();
  });

  it("reports partial and unknown retained counts without presenting zero as a verified result", async () => {
    mocks.clearCache.mockResolvedValue(
      snapshot({
        clearRequest: receipt({
          state: "completed-with-retained-items",
          retainedFiles: 4,
          retainedBytes: 60 * mb,
          retainedCountsKnown: true,
          reasons: ["Pinned output is retained"],
        }),
      }),
    );
    await mount();
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Clear preview cache" }),
      );
    });
    expect(screen.getByRole("status").className).not.toBe("success");
    expect(screen.getByText(/4 retained cache files/)).toBeTruthy();
    expect(screen.getByText("Pinned output is retained")).toBeTruthy();
    mocks.clearCache.mockResolvedValue(
      snapshot({
        clearRequest: receipt({
          requestId: "new-clear",
          generation: "8",
          state: "failed",
          retainedCountsKnown: false,
          reasons: ["Inventory could not be enumerated"],
        }),
      }),
    );
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Clear preview cache" }),
      );
    });
    expect(
      screen.getByText("Retained cache counts are not yet verified."),
    ).toBeTruthy();
    expect(screen.queryByText(/0 retained cache files/)).toBeNull();
  });

  it("retries the exact failed request and reports only admission until the linked request completes", async () => {
    const failed = receipt({
      state: "failed",
      reasons: ["Original deletion failure"],
    });
    mocks.cacheInfo.mockResolvedValue(snapshot({ clearRequest: failed }));
    const admission = deferred<CacheInfo>();
    mocks.clearCache.mockReturnValue(admission.promise);
    await mount();
    const retry = screen.getByRole("button", { name: "Retry clear request" });
    fireEvent.click(retry);
    fireEvent.click(retry);
    expect(mocks.clearCache).toHaveBeenCalledExactlyOnceWith("clear-request-A");
    expect((retry as HTMLButtonElement).disabled).toBe(true);
    expect(screen.queryByText(/Clear request accepted/)).toBeNull();
    const retried = receipt({
      requestId: "retry-request-B",
      retryOf: "clear-request-A",
    });
    await act(async () => {
      admission.resolve(
        snapshot({ maintenancePending: true, clearRequest: retried }),
      );
    });
    expect(screen.getByRole("status").textContent).toContain(
      "Clear request accepted",
    );
    expect(screen.queryByText(/Unused cache entries cleared/)).toBeNull();
    mocks.cacheInfo.mockResolvedValue(
      snapshot({
        clearRequest: {
          ...retried,
          state: "completed",
          retainedCountsKnown: true,
        },
      }),
    );
    await tick();
    expect(mocks.cacheInfo).toHaveBeenLastCalledWith(false, "retry-request-B");
    expect(screen.getByRole("status").textContent).toContain(
      "Unused cache entries cleared",
    );
  });

  it.each([
    [
      "wrong retry parent",
      receipt({
        ...complete(),
        requestId: "retry-B",
        retryOf: "unrelated-request",
      }),
    ],
    [
      "expanded retry generation",
      receipt({
        ...complete(),
        requestId: "retry-B",
        retryOf: "clear-request-A",
        generation: "8",
      }),
    ],
  ])(
    "does not accept a %s as retry completion",
    async (_name, clearRequest) => {
      mocks.cacheInfo.mockResolvedValue(
        snapshot({
          clearRequest: receipt({
            state: "failed",
            reasons: ["Preserved failure"],
          }),
        }),
      );
      mocks.clearCache.mockResolvedValue(snapshot({ clearRequest }));
      await mount();
      await act(async () => {
        fireEvent.click(
          screen.getByRole("button", { name: "Retry clear request" }),
        );
      });
      expect(screen.getByRole("status").textContent).toContain(
        "outcome is unconfirmed",
      );
      expect(screen.getByText("Preserved failure")).toBeTruthy();
      expect(screen.queryByText(/Unused cache entries cleared/)).toBeNull();
    },
  );

  it("never adopts an older completed receipt after a clear response omits admission", async () => {
    mocks.cacheInfo.mockResolvedValue(snapshot({ clearRequest: complete() }));
    mocks.clearCache.mockResolvedValue(snapshot());
    await mount();
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Clear preview cache" }),
      );
    });
    await tick();
    expect(screen.getByRole("status").textContent).toContain(
      "outcome is unconfirmed",
    );
    expect(screen.queryByText(/Unused cache entries cleared/)).toBeNull();
  });

  it("does not reuse the previous successful receipt as admission for a new clear", async () => {
    mocks.cacheInfo.mockResolvedValue(snapshot({ clearRequest: complete() }));
    mocks.clearCache.mockResolvedValue(snapshot({ clearRequest: complete() }));
    await mount();
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Clear preview cache" }),
      );
    });
    expect(screen.getByRole("status").textContent).toContain(
      "outcome is unconfirmed",
    );
    expect(screen.queryByText(/Unused cache entries cleared/)).toBeNull();
  });

  it("keeps a rejected retry unconfirmed when refresh only finds the earlier failed request", async () => {
    const failed = receipt({
      state: "failed",
      reasons: ["Original cleanup failure"],
    });
    mocks.cacheInfo.mockResolvedValue(snapshot({ clearRequest: failed }));
    mocks.clearCache.mockRejectedValue(
      new Error("CACHE_RETRY_ADMISSION_FAILED"),
    );
    await mount();
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Retry clear request" }),
      );
    });
    expect(screen.getByRole("alert").textContent).toContain(
      "CACHE_RETRY_ADMISSION_FAILED",
    );
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Refresh cache status" }),
      );
    });
    expect(screen.getByRole("status").textContent).toContain(
      "outcome is unconfirmed",
    );
    expect(screen.getByText("Original cleanup failure")).toBeTruthy();
    expect(
      screen.queryByText(/Clear request accepted|Unused cache entries cleared/),
    ).toBeNull();
  });

  it("shows native retirement warnings even after dispatcher owner removal", async () => {
    mocks.clearCache.mockResolvedValue(
      snapshot({
        retirementIssues: [
          "CACHE_RETIREMENT_IO: owned file could not be removed",
        ],
        clearRequest: receipt({ state: "failed", retainedCountsKnown: false }),
      }),
    );
    await mount();
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Clear preview cache" }),
      );
    });
    expect(
      screen.getByLabelText("Cache retirement warnings").textContent,
    ).toContain("CACHE_RETIREMENT_IO");
    expect(screen.queryByText(/Unused cache entries cleared/)).toBeNull();
  });

  it("keeps unknown inventory distinct from zero files and reports running maintenance", async () => {
    mocks.cacheInfo.mockResolvedValue(
      snapshot({
        files: 0,
        sizeBytes: 0,
        inventoryKnown: false,
        maintenanceRunning: true,
      }),
    );
    await mount();
    expect(
      screen.getByText(/Cache inventory is not yet available/),
    ).toBeTruthy();
    expect(screen.queryByText(/in 0 files/)).toBeNull();
    expect(screen.getByRole("status").textContent).toContain(
      "maintenance is running",
    );
    expect(screen.getByText(/2 displayed or prefetched previews/)).toBeTruthy();
    mocks.cacheInfo.mockResolvedValue(
      snapshot({ files: 0, sizeBytes: 0, pinnedFiles: 0, pinnedBytes: 0 }),
    );
    await tick();
    expect(screen.getByText(/in 0 files/)).toBeTruthy();
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("does not overlap polls or let a delayed read overwrite a new budget", async () => {
    const oldRead = deferred<CacheInfo>();
    mocks.cacheInfo
      .mockResolvedValueOnce(snapshot({ maintenancePending: true }))
      .mockReturnValueOnce(oldRead.promise);
    await mount();
    await tick();
    await tick(5000);
    expect(mocks.cacheInfo).toHaveBeenCalledTimes(2);
    mocks.setCacheLimit.mockResolvedValue(snapshot({ budgetBytes: 512 * mb }));
    await act(async () => {
      fireEvent.change(screen.getByLabelText("Cache budget"), {
        target: { value: String(512 * mb) },
      });
    });
    expect(
      (screen.getByLabelText("Cache budget") as HTMLSelectElement).value,
    ).toBe(String(512 * mb));
    await act(async () => {
      oldRead.resolve(
        snapshot({ budgetBytes: 128 * mb, maintenancePending: true }),
      );
    });
    await tick(5000);
    expect(
      (screen.getByLabelText("Cache budget") as HTMLSelectElement).value,
    ).toBe(String(512 * mb));
    expect(mocks.cacheInfo).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("ignores a stale poll error after a successful mutation", async () => {
    const oldRead = deferred<CacheInfo>();
    mocks.cacheInfo
      .mockResolvedValueOnce(snapshot({ maintenancePending: true }))
      .mockReturnValueOnce(oldRead.promise);
    await mount();
    await tick();
    mocks.setCacheLimit.mockResolvedValue(snapshot({ budgetBytes: 512 * mb }));
    await act(async () => {
      fireEvent.change(screen.getByLabelText("Cache budget"), {
        target: { value: String(512 * mb) },
      });
    });
    await act(async () => {
      oldRead.reject(new Error("obsolete read"));
    });
    expect(screen.queryByRole("alert")).toBeNull();
    expect(
      (screen.getByLabelText("Cache budget") as HTMLSelectElement).value,
    ).toBe(String(512 * mb));
  });

  it("stops automatic refresh on failure and confirms a clear only after visible retry succeeds", async () => {
    await mount();
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Clear preview cache" }),
      );
    });
    mocks.cacheInfo.mockRejectedValue(
      new Error("Cache status temporarily unavailable"),
    );
    await tick();
    expect(screen.getByRole("alert").textContent).toContain(
      "temporarily unavailable",
    );
    expect(screen.getByRole("status").textContent).toContain(
      "Refresh to confirm",
    );
    expect(screen.queryByText(/Unused cache entries cleared/)).toBeNull();
    await tick(5000);
    expect(mocks.cacheInfo).toHaveBeenCalledTimes(2);
    mocks.cacheInfo.mockResolvedValue(
      snapshot({ files: 2, sizeBytes: 240 * mb, clearRequest: complete() }),
    );
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Refresh cache status" }),
      );
    });
    expect(mocks.cacheInfo).toHaveBeenLastCalledWith(true, "clear-request-A");
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("status").textContent).toContain(
      "Unused cache entries cleared",
    );
  });

  it("bounds automatic refresh and lets the user start another status cycle", async () => {
    mocks.cacheInfo.mockResolvedValue(
      snapshot({ maintenancePending: true, inventoryKnown: false }),
    );
    await mount();
    for (let count = 0; count < 30; count += 1) await tick();
    expect(mocks.cacheInfo).toHaveBeenCalledTimes(31);
    expect(screen.getByText(/Automatic refresh is paused/)).toBeTruthy();
    await tick(10000);
    expect(mocks.cacheInfo).toHaveBeenCalledTimes(31);
    mocks.cacheInfo.mockResolvedValue(snapshot());
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Refresh cache status" }),
      );
    });
    expect(mocks.cacheInfo).toHaveBeenCalledTimes(32);
    expect(screen.queryByText(/Automatic refresh is paused/)).toBeNull();
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("cancels scheduled polls on unmount and ignores a late in-flight result", async () => {
    mocks.cacheInfo.mockResolvedValue(snapshot({ maintenancePending: true }));
    const view = render(<CacheSettings />);
    await act(async () => {});
    const pending = deferred<CacheInfo>();
    mocks.cacheInfo.mockReturnValueOnce(pending.promise);
    await tick();
    expect(mocks.cacheInfo).toHaveBeenCalledTimes(2);
    view.unmount();
    await act(async () => {
      pending.resolve(snapshot({ maintenancePending: true }));
    });
    await tick(10000);
    expect(mocks.cacheInfo).toHaveBeenCalledTimes(2);
    expect(screen.queryByText("Preview cache")).toBeNull();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("keeps mutations serialized and ignores completion after unmount", async () => {
    const mutation = deferred<CacheInfo>();
    mocks.clearCache.mockReturnValue(mutation.promise);
    const view = render(<CacheSettings />);
    await act(async () => {});
    const clear = screen.getByRole("button", { name: "Clear preview cache" });
    fireEvent.click(clear);
    fireEvent.click(clear);
    expect(mocks.clearCache).toHaveBeenCalledTimes(1);
    expect(
      (screen.getByLabelText("Cache budget") as HTMLSelectElement).disabled,
    ).toBe(true);
    view.unmount();
    await act(async () => {
      mutation.resolve(snapshot({ maintenancePending: true }));
    });
    await tick(10000);
    expect(mocks.cacheInfo).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });
});
