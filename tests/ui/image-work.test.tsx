// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ImageWorkSettings } from "../../src/components/ImageWorkSettings";
import type { ImageWorkStatus } from "../../src/native";
const mock = vi.hoisted(() => ({
  desktop: true,
  imageWorkStatus: vi.fn(),
  cancelImageWork: vi.fn(),
  reapImageWorker: vi.fn(),
  retryCacheMaintenance: vi.fn(),
}));
vi.mock("../../src/native", () => ({
  get desktop() {
    return mock.desktop;
  },
  api: mock,
}));
const idle: ImageWorkStatus = {
  active: 0,
  limit: 18,
  jobs: [],
  decoder: {
    active: 0,
    pending: 0,
    reservedBytes: 0,
    liveOutputs: 0,
    outputReservedBytes: 0,
    unavailable: false,
    workerPid: null,
    stage: null,
    recoveryAction: null,
  },
};
const running: ImageWorkStatus = {
  ...idle,
  active: 1,
  jobs: [
    {
      requestId: "request-A",
      contextId: "native-A",
      kind: "scan",
      phase: "scanning",
      cancelled: false,
    },
  ],
  decoder: {
    ...idle.decoder,
    active: 1,
    reservedBytes: 1048576,
    stage: "decoding",
    workerPid: 123,
  },
};
const unavailable: ImageWorkStatus = {
  ...idle,
  decoder: {
    ...idle.decoder,
    unavailable: true,
    reservedBytes: 1048576,
    recoveryAction: "retry-reap",
  },
};
const outputCleanup: ImageWorkStatus = {
  ...idle,
  decoder: {
    ...idle.decoder,
    unavailable: true,
    liveOutputs: 1,
    outputReservedBytes: 2048,
    recoveryAction: "retry-output-cleanup",
  },
};
const retiredCache: NonNullable<ImageWorkStatus["cacheMaintenance"]> = {
  active: false,
  ownerLimit: 2,
  owners: [
    {
      requestId: "retained-old-profile-cache",
      phase: "retry-required",
      cancelled: false,
      runnable: false,
      retirementComplete: false,
      lastError: "CACHE_DELETE_RETRY: original cache still owns its cleanup",
      usage: {
        sizeBytes: 2048,
        files: 2,
        budgetBytes: 4096,
        pinnedFiles: 0,
        pinnedBytes: 0,
        pendingDeleteFiles: 2,
        pendingPreviews: 0,
        pixelReservedBytes: 0,
        retirementIssues: [],
        temporaryFiles: 0,
        temporaryBytes: 0,
        clearRequest: null,
        inventoryKnown: true,
        maintenancePending: true,
        maintenanceRunning: false,
      },
    },
  ],
};
beforeEach(() => {
  vi.resetAllMocks();
  mock.desktop = true;
  mock.imageWorkStatus.mockResolvedValue(idle);
});
afterEach(cleanup);
describe("independent native image controls", () => {
  it("offers the native temporary-output retry without describing it as a worker-exit problem", async () => {
    mock.imageWorkStatus.mockResolvedValue(outputCleanup);
    render(<ImageWorkSettings />);
    await screen.findByRole("button", {
      name: "Retry temporary output cleanup",
    });
    expect(
      screen.getByText(/Temporary output cleanup is unconfirmed/),
    ).toBeTruthy();
    expect(
      screen.queryByText(/until native worker exit is verified/),
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Retry native worker cleanup" }),
    ).toBeNull();
  });
  it("does not report worker-only recovery as complete cleanup while derived output ownership remains", async () => {
    mock.imageWorkStatus.mockResolvedValue(unavailable);
    const retained = {
      ...idle,
      decoder: { ...idle.decoder, liveOutputs: 1, outputReservedBytes: 2048 },
    };
    mock.reapImageWorker.mockResolvedValue(retained.decoder);
    render(<ImageWorkSettings />);
    const retry = await screen.findByRole("button", {
      name: "Retry native worker cleanup",
    });
    mock.imageWorkStatus.mockResolvedValue(retained);
    fireEvent.click(retry);
    await screen.findByText(/1 derived output remains retained/);
    expect(
      screen.queryByText(
        "Native worker cleanup completed. Image work can be retried.",
      ),
    ).toBeNull();
    expect(screen.getByText(/Derived outputs: 1/)).toBeTruthy();
  });
  it("waits for native temporary-output cleanup and prevents duplicate recovery requests", async () => {
    mock.imageWorkStatus.mockResolvedValue(outputCleanup);
    let finish!: (value: ImageWorkStatus["decoder"]) => void;
    mock.reapImageWorker.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    render(<ImageWorkSettings />);
    const retry = await screen.findByRole("button", {
      name: "Retry temporary output cleanup",
    });
    fireEvent.click(retry);
    fireEvent.click(retry);
    expect(mock.reapImageWorker).toHaveBeenCalledExactlyOnceWith();
    expect((retry as HTMLButtonElement).disabled).toBe(true);
    expect(
      (
        screen.getByRole("button", {
          name: "Refresh image work",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
    expect(screen.queryByRole("status")).toBeNull();
    mock.imageWorkStatus.mockResolvedValue(idle);
    await act(async () => {
      finish(idle.decoder);
    });
    await screen.findByText(
      "Temporary output cleanup completed. Image work can be retried.",
    );
    expect(
      screen.queryByRole("button", { name: "Retry temporary output cleanup" }),
    ).toBeNull();
    expect(screen.queryByText(/Native worker cleanup completed/)).toBeNull();
  });
  it("retains unconfirmed temporary outputs when native cleanup still cannot mark deletion", async () => {
    mock.imageWorkStatus.mockResolvedValue(outputCleanup);
    mock.reapImageWorker.mockResolvedValue(outputCleanup.decoder);
    render(<ImageWorkSettings />);
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Retry temporary output cleanup",
      }),
    );
    await screen.findByText(
      "Temporary output cleanup remains unconfirmed. The output and its reservation remain retained.",
    );
    expect(screen.queryByText(/cleanup completed/)).toBeNull();
    expect(screen.getByText(/Derived outputs: 1/)).toBeTruthy();
    await waitFor(() =>
      expect(
        (
          screen.getByRole("button", {
            name: "Retry temporary output cleanup",
          }) as HTMLButtonElement
        ).disabled,
      ).toBe(false),
    );
  });
  it("qualifies a successful output retry while a protection guard still owns its capacity", async () => {
    mock.imageWorkStatus.mockResolvedValue(outputCleanup);
    const retained = {
      ...idle,
      decoder: { ...idle.decoder, liveOutputs: 1, outputReservedBytes: 2048 },
    };
    mock.reapImageWorker.mockResolvedValue(retained.decoder);
    render(<ImageWorkSettings />);
    const retry = await screen.findByRole("button", {
      name: "Retry temporary output cleanup",
    });
    mock.imageWorkStatus.mockResolvedValue(retained);
    fireEvent.click(retry);
    const message = await screen.findByRole("status");
    expect(message.textContent).toMatch(
      /Temporary output cleanup retry finished\. 1 derived output remains retained/,
    );
    expect(message.textContent).toContain("including protection checks");
    expect(screen.queryByText(/Image work can be retried/)).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Retry temporary output cleanup" }),
    ).toBeNull();
  });
  it("does not erase temporary-output retry failure with a successful observational refresh", async () => {
    mock.imageWorkStatus.mockResolvedValue(outputCleanup);
    mock.reapImageWorker.mockRejectedValue(
      new Error("DECODE_OUTPUT_DELETE_UNCONFIRMED"),
    );
    render(<ImageWorkSettings />);
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Retry temporary output cleanup",
      }),
    );
    await screen.findByText(/DECODE_OUTPUT_DELETE_UNCONFIRMED/);
    fireEvent.click(screen.getByRole("button", { name: "Refresh image work" }));
    await waitFor(() => expect(mock.imageWorkStatus).toHaveBeenCalledTimes(2));
    expect(screen.getByText(/DECODE_OUTPUT_DELETE_UNCONFIRMED/)).toBeTruthy();
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.getByText(/Derived outputs: 1/)).toBeTruthy();
  });
  it("does not apply an output cleanup response or refresh after unmount", async () => {
    let finish!: (value: ImageWorkStatus["decoder"]) => void;
    mock.imageWorkStatus.mockResolvedValue(outputCleanup);
    mock.reapImageWorker.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const view = render(<ImageWorkSettings />);
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Retry temporary output cleanup",
      }),
    );
    view.unmount();
    await act(async () => {
      finish(idle.decoder);
    });
    expect(mock.imageWorkStatus).toHaveBeenCalledTimes(1);
    expect(screen.queryByLabelText("Native image work")).toBeNull();
  });
  it("retries the exact retained cache owner and keeps actions disabled until native admission", async () => {
    mock.imageWorkStatus.mockResolvedValue({
      ...idle,
      cacheMaintenance: retiredCache,
    });
    let finish!: (
      value: NonNullable<ImageWorkStatus["cacheMaintenance"]>,
    ) => void;
    mock.retryCacheMaintenance.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    render(<ImageWorkSettings />);
    const retry = await screen.findByRole("button", {
      name: "Retry cache maintenance",
    });
    fireEvent.click(retry);
    fireEvent.click(retry);
    expect(mock.retryCacheMaintenance).toHaveBeenCalledExactlyOnceWith(
      "retained-old-profile-cache",
    );
    expect((retry as HTMLButtonElement).disabled).toBe(true);
    expect(
      (
        screen.getByRole("button", {
          name: "Refresh image work",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
    expect(screen.queryByText(/Cache maintenance retry requested/)).toBeNull();
    const queued = {
      ...retiredCache,
      active: true,
      owners: retiredCache.owners.map((owner) => ({
        ...owner,
        phase: "queued",
        runnable: true,
        lastError: null,
      })),
    };
    mock.imageWorkStatus.mockResolvedValue({
      ...idle,
      cacheMaintenance: queued,
    });
    await act(async () => {
      finish(queued);
    });
    await screen.findByText(/Cache maintenance retry requested/);
    expect(screen.queryByText(/cleanup completed/)).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Retry cache maintenance" }),
    ).toBeNull();
  });
  it("retains the failed owner and retry error when a refresh does not resolve cleanup", async () => {
    mock.imageWorkStatus.mockResolvedValue({
      ...idle,
      cacheMaintenance: retiredCache,
    });
    mock.retryCacheMaintenance.mockRejectedValue(
      new Error("CACHE_RETRY_UNAVAILABLE"),
    );
    render(<ImageWorkSettings />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Retry cache maintenance" }),
    );
    await screen.findByText(/CACHE_RETRY_UNAVAILABLE/);
    fireEvent.click(screen.getByRole("button", { name: "Refresh image work" }));
    await waitFor(() => expect(mock.imageWorkStatus).toHaveBeenCalledTimes(2));
    expect(screen.getByText(/CACHE_DELETE_RETRY/)).toBeTruthy();
    expect(screen.getByText(/CACHE_RETRY_UNAVAILABLE/)).toBeTruthy();
    expect(
      (
        screen.getByRole("button", {
          name: "Retry cache maintenance",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(false);
    expect(screen.queryByText(/retry requested|cleanup completed/i)).toBeNull();
  });
  it("waits for cancelled maintenance to exit before offering a usable retry", async () => {
    const owner = {
      ...retiredCache.owners[0],
      phase: "cancelled",
      cancelled: true,
    };
    mock.imageWorkStatus.mockResolvedValue({
      ...idle,
      cacheMaintenance: {
        ...retiredCache,
        owners: [
          { ...owner, usage: { ...owner.usage, maintenanceRunning: true } },
        ],
      },
    });
    render(<ImageWorkSettings />);
    const retry = await screen.findByRole("button", {
      name: "Retry cache maintenance",
    });
    expect((retry as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(retry);
    expect(mock.retryCacheMaintenance).not.toHaveBeenCalled();
    mock.imageWorkStatus.mockResolvedValue({
      ...idle,
      cacheMaintenance: { ...retiredCache, owners: [owner] },
    });
    fireEvent.click(screen.getByRole("button", { name: "Refresh image work" }));
    await waitFor(() =>
      expect((retry as HTMLButtonElement).disabled).toBe(false),
    );
  });
  it("ignores an older status read that completes after the owner's retry result", async () => {
    let older!: (value: ImageWorkStatus) => void;
    mock.imageWorkStatus
      .mockResolvedValueOnce({ ...idle, cacheMaintenance: retiredCache })
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            older = resolve;
          }),
      )
      .mockResolvedValue(idle);
    const queued = {
      ...retiredCache,
      active: true,
      owners: retiredCache.owners.map((owner) => ({
        ...owner,
        phase: "queued",
        runnable: true,
        lastError: null,
      })),
    };
    mock.retryCacheMaintenance.mockResolvedValue(queued);
    render(<ImageWorkSettings />);
    const retry = await screen.findByRole("button", {
      name: "Retry cache maintenance",
    });
    fireEvent.click(screen.getByRole("button", { name: "Refresh image work" }));
    fireEvent.click(retry);
    await screen.findByText(/Cache maintenance retry requested/);
    await waitFor(() => expect(mock.imageWorkStatus).toHaveBeenCalledTimes(3));
    await act(async () => {
      older({ ...idle, cacheMaintenance: retiredCache });
    });
    expect(
      screen.queryByRole("button", { name: "Retry cache maintenance" }),
    ).toBeNull();
    expect(screen.queryByText(/CACHE_DELETE_RETRY/)).toBeNull();
  });
  it("does not refresh or apply a retained-owner retry after unmount", async () => {
    let finish!: (
      value: NonNullable<ImageWorkStatus["cacheMaintenance"]>,
    ) => void;
    mock.imageWorkStatus.mockResolvedValue({
      ...idle,
      cacheMaintenance: retiredCache,
    });
    mock.retryCacheMaintenance.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const view = render(<ImageWorkSettings />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Retry cache maintenance" }),
    );
    view.unmount();
    await act(async () => {
      finish(retiredCache);
    });
    expect(mock.imageWorkStatus).toHaveBeenCalledTimes(1);
    expect(screen.queryByLabelText("Native image work")).toBeNull();
  });
  it("offers cancellation when only context-independent cache maintenance is running", async () => {
    const owner = {
      requestId: "cache-owner",
      phase: "maintaining-cache",
      cancelled: false,
      runnable: true,
      retirementComplete: false,
      lastError: null,
      usage: {
        sizeBytes: 2048,
        files: 2,
        budgetBytes: 4096,
        pinnedFiles: 1,
        pinnedBytes: 1024,
        pendingDeleteFiles: 1,
        pendingPreviews: 0,
        pixelReservedBytes: 1024,
        retirementIssues: [],
        temporaryFiles: 0,
        temporaryBytes: 0,
        clearRequest: null,
        inventoryKnown: true,
        maintenancePending: true,
        maintenanceRunning: true,
      },
    };
    mock.imageWorkStatus.mockResolvedValue({
      ...idle,
      cacheMaintenance: { active: true, ownerLimit: 2, owners: [owner] },
    });
    mock.cancelImageWork.mockResolvedValue(1);
    render(<ImageWorkSettings />);
    await screen.findByText(/Cache maintenance retains at most 2/);
    const cancel = screen.getByRole("button", {
      name: "Cancel image work",
    }) as HTMLButtonElement;
    expect(cancel.disabled).toBe(false);
    mock.imageWorkStatus.mockResolvedValue({
      ...idle,
      cacheMaintenance: {
        active: false,
        ownerLimit: 2,
        owners: [{ ...owner, phase: "cancelled", cancelled: true }],
      },
    });
    fireEvent.click(cancel);
    await screen.findByText(/Work remains reserved until it exits/);
    expect(mock.cancelImageWork).toHaveBeenCalledOnce();
    await waitFor(() => expect(cancel.disabled).toBe(true));
  });
  it("keeps cancelled work reserved until native status proves it exited", async () => {
    mock.imageWorkStatus.mockResolvedValue(running);
    mock.cancelImageWork.mockResolvedValue(1);
    render(<ImageWorkSettings />);
    await screen.findByText("1 active image requests · 18 request limit");
    const cancelled = {
      ...running,
      jobs: [{ ...running.jobs[0], cancelled: true }],
    };
    mock.imageWorkStatus.mockResolvedValue(cancelled);
    fireEvent.click(screen.getByRole("button", { name: "Cancel image work" }));
    await screen.findByText(/Work remains reserved until it exits/);
    expect(
      screen.getByText("1 active image requests · 18 request limit"),
    ).toBeTruthy();
    expect(
      screen.getByText(/Cancellation requested; waiting for exit/),
    ).toBeTruthy();
    expect(mock.cancelImageWork).toHaveBeenCalledTimes(1);
    mock.imageWorkStatus.mockResolvedValue(idle);
    fireEvent.click(screen.getByRole("button", { name: "Refresh image work" }));
    await screen.findByText("0 active image requests · 18 request limit");
    expect(
      (
        screen.getByRole("button", {
          name: "Cancel image work",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
  });
  it("only reports successful worker cleanup after the native result and refreshed status", async () => {
    mock.imageWorkStatus.mockResolvedValue(unavailable);
    let finish!: (value: ImageWorkStatus["decoder"]) => void;
    mock.reapImageWorker.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    render(<ImageWorkSettings />);
    const button = await screen.findByRole("button", {
      name: "Retry native worker cleanup",
    });
    fireEvent.click(button);
    expect((button as HTMLButtonElement).disabled).toBe(true);
    expect(screen.queryByText(/cleanup completed/)).toBeNull();
    mock.imageWorkStatus.mockResolvedValue(idle);
    await act(async () => {
      finish(idle.decoder);
    });
    await screen.findByText(
      "Native worker cleanup completed. Image work can be retried.",
    );
    expect(
      screen.queryByRole("button", { name: "Retry native worker cleanup" }),
    ).toBeNull();
  });
  it("retains a failed cleanup explanation across independent status refreshes", async () => {
    mock.imageWorkStatus.mockResolvedValue(unavailable);
    mock.reapImageWorker.mockRejectedValue(
      new Error("DECODE_REAP_UNCONFIRMED"),
    );
    render(<ImageWorkSettings />);
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Retry native worker cleanup",
      }),
    );
    await screen.findByText(/DECODE_REAP_UNCONFIRMED/);
    fireEvent.click(screen.getByRole("button", { name: "Refresh image work" }));
    await waitFor(() => expect(mock.imageWorkStatus).toHaveBeenCalledTimes(2));
    expect(screen.getByText(/DECODE_REAP_UNCONFIRMED/)).toBeTruthy();
    expect(screen.queryByText(/cleanup completed/)).toBeNull();
  });
  it("explains restart-required ownership failure without offering a fake reset", async () => {
    mock.imageWorkStatus.mockResolvedValue({
      ...unavailable,
      decoder: {
        ...unavailable.decoder,
        recoveryAction: "restart-application",
      },
    });
    render(<ImageWorkSettings />);
    await screen.findByText(
      /Save or export pending work, then close and reopen/,
    );
    expect(
      screen.queryByRole("button", { name: "Retry native worker cleanup" }),
    ).toBeNull();
  });
  it("ignores an older status response that arrives after a newer refresh", async () => {
    let initial!: (value: ImageWorkStatus) => void;
    mock.imageWorkStatus
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            initial = resolve;
          }),
      )
      .mockResolvedValue(running);
    render(<ImageWorkSettings />);
    fireEvent.click(screen.getByRole("button", { name: "Refresh image work" }));
    await screen.findByText("1 active image requests · 18 request limit");
    await act(async () => {
      initial(idle);
    });
    expect(
      screen.getByText("1 active image requests · 18 request limit"),
    ).toBeTruthy();
  });
  it("does not continue polling after unmount when a status request completes late", async () => {
    let finish!: (value: ImageWorkStatus) => void;
    mock.imageWorkStatus.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const view = render(<ImageWorkSettings />);
    view.unmount();
    await act(async () => {
      finish(running);
    });
    expect(mock.imageWorkStatus).toHaveBeenCalledTimes(1);
    expect(screen.queryByLabelText("Native image work")).toBeNull();
  });
  it("keeps native worker actions out of the browser demo", () => {
    mock.desktop = false;
    render(<ImageWorkSettings />);
    expect(mock.imageWorkStatus).not.toHaveBeenCalled();
    expect(screen.queryByLabelText("Native image work")).toBeNull();
  });
});

it("labels the native file mutation reservation without inventing a decoder job", async () => {
  mock.imageWorkStatus.mockResolvedValue({
    ...idle,
    active: 1,
    jobs: [
      {
        requestId: "file-change-guard",
        contextId: "native-A",
        kind: "file-mutation",
        phase: "committing",
        cancelled: false,
      },
    ],
  });
  render(<ImageWorkSettings />);
  await screen.findByText(/File changes · preview admission paused/);
  expect(screen.queryByText(/worker PID/)).toBeNull();
});
