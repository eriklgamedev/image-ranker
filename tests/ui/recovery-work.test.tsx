// @vitest-environment jsdom
import { StrictMode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RecoveryWorkControls } from "../../src/components/RecoveryWorkControls";
import { StartupRecovery } from "../../src/components/StartupRecovery";
import type { RecoveryWorkJob, RecoveryWorkStatus } from "../../src/native";

const job: RecoveryWorkJob = {
  id: "native-recovery-A",
  contextId: "context-A",
  profileGeneration: "profile-A",
  kind: "cleanup",
  phase: "walking staged files",
  state: "running",
  completed: 3,
  total: 9,
  cancelRequested: false,
  message: null,
};
const idle: RecoveryWorkStatus = { active: null, recent: [], limit: 1 };
const active: RecoveryWorkStatus = { ...idle, active: job };
const commands = () => ({
  status: vi.fn().mockResolvedValue(active),
  cancel: vi.fn().mockResolvedValue({ id: job.id, cancelRequested: true }),
});
function deferred<T>() {
  let resolve!: (value: T) => void, reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
async function tick(milliseconds = 1000) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(milliseconds);
  });
}
beforeEach(() => vi.useFakeTimers());
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("independent recovery work controls", () => {
  it("retries StrictMode's ignored initial read after it settles even when watch is false", async () => {
    const actions = commands();
    const first = deferred<RecoveryWorkStatus>();
    actions.status.mockReturnValueOnce(first.promise);
    render(
      <StrictMode>
        <RecoveryWorkControls actions={actions} watch={false} />
      </StrictMode>,
    );
    expect(actions.status).toHaveBeenCalledTimes(1);
    await act(async () => {
      first.resolve(active);
    });
    await tick();
    expect(actions.status).toHaveBeenCalledTimes(2);
    expect(
      screen.getByRole("button", { name: "Cancel recovery work" }),
    ).toBeTruthy();
  });

  it("preserves a failed cancel acknowledgment across a successful status refresh", async () => {
    const actions = commands();
    actions.cancel.mockRejectedValue(new Error("RECOVERY_CANCEL_REJECTED"));
    await act(async () => {
      render(<RecoveryWorkControls actions={actions} watch={false} />);
    });
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Cancel recovery work" }),
      );
    });
    expect(screen.getByRole("alert").textContent).toContain(
      "RECOVERY_CANCEL_REJECTED",
    );
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Refresh recovery work status" }),
      );
    });
    expect(screen.getByRole("alert").textContent).toContain(
      "RECOVERY_CANCEL_REJECTED",
    );
  });

  it("blocks startup actions for native work found on remount while keeping cancellation available", async () => {
    const actions = commands();
    const retry = vi.fn();
    const onBusyChange = vi.fn();
    await act(async () => {
      render(
        <StartupRecovery
          status={{ mode: "recovery", issues: [], canRestoreBackup: true }}
          failure=""
          onRetry={retry}
          onRestored={() => {}}
          onBusyChange={onBusyChange}
          work={actions}
        />,
      );
    });
    expect(
      (
        screen.getByRole("button", {
          name: "Retry loading",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
    expect(
      (
        screen.getByRole("button", {
          name: "Cancel recovery work",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(false);
    expect(onBusyChange).toHaveBeenLastCalledWith(true);
  });

  it("loads retained recovery requests once initial native status proves work is idle", async () => {
    const actions = commands();
    const initial = deferred<RecoveryWorkStatus>();
    actions.status.mockReturnValueOnce(initial.promise).mockResolvedValue(idle);
    const storage = {
      loadStatus: vi.fn().mockResolvedValue({
        selectedRequestId: null,
        selectedBranchId: null,
        activationState: "original",
        usedBranches: 0,
        usedBytes: 0,
        limits: { branches: 8, bytes: 2147483648, requests: 1024 },
        requests: [],
        warnings: [],
      }),
      prepareResume: vi.fn(),
      confirmResume: vi.fn(),
      cancelResume: vi.fn(),
      previewCleanup: vi.fn(),
      confirmCleanup: vi.fn(),
      cancelCleanup: vi.fn(),
    };
    await act(async () => {
      render(
        <StartupRecovery
          status={{ mode: "recovery", issues: [], canRestoreBackup: true }}
          failure=""
          onRetry={async () => {}}
          onRestored={() => {}}
          work={actions}
          storage={storage}
        />,
      );
    });
    expect(storage.loadStatus).not.toHaveBeenCalled();
    expect(
      (
        screen.getByRole("button", {
          name: "Refresh recovery status",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
    await act(async () => {
      initial.resolve(idle);
    });
    expect(storage.loadStatus).toHaveBeenCalledOnce();
    expect(
      (
        screen.getByRole("button", {
          name: "Refresh recovery status",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(false);
    await tick(10000);
    expect(storage.loadStatus).toHaveBeenCalledOnce();
  });

  it("keeps status and exact-job cancellation usable while a parent recovery promise is unsettled", async () => {
    const actions = commands();
    actions.status.mockResolvedValue(idle);
    const operation = deferred<void>();
    const retry = vi.fn(() => operation.promise);
    const onBusyChange = vi.fn();
    await act(async () => {
      render(
        <StartupRecovery
          status={{ mode: "recovery", issues: [], canRestoreBackup: true }}
          failure=""
          onRetry={retry}
          onRestored={() => {}}
          onBusyChange={onBusyChange}
          work={actions}
        />,
      );
    });
    actions.status.mockResolvedValue(active);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Retry loading" }));
    });
    expect(retry).toHaveBeenCalledOnce();
    expect(screen.getByText(/walking staged files/)).toBeTruthy();
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Cancel recovery work" }),
      );
    });
    expect(actions.cancel).toHaveBeenCalledExactlyOnceWith(job.id);
    expect(screen.getByText(/safe boundary before it stops/)).toBeTruthy();
    expect(onBusyChange).toHaveBeenLastCalledWith(true);
    await act(async () => {
      operation.resolve();
    });
    expect(
      (
        screen.getByRole("button", {
          name: "Retry loading",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
    actions.status.mockResolvedValue({
      ...idle,
      recent: [{ ...job, state: "cancelled" }],
    });
    await tick();
    expect(onBusyChange).toHaveBeenLastCalledWith(false);
    expect(
      (
        screen.getByRole("button", {
          name: "Retry loading",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(false);
  });

  it.each([
    { id: "different-job", cancelRequested: true },
    { id: job.id, cancelRequested: false },
  ])(
    "rejects an acknowledgment that does not confirm this exact cancellation: %j",
    async (ack) => {
      const actions = commands();
      actions.cancel.mockResolvedValue(ack);
      await act(async () => {
        render(<RecoveryWorkControls actions={actions} watch={false} />);
      });
      await act(async () => {
        fireEvent.click(
          screen.getByRole("button", { name: "Cancel recovery work" }),
        );
      });
      expect(screen.getByRole("alert").textContent).toContain(
        "not acknowledged for this job",
      );
      expect(screen.queryByText(/safe boundary before it stops/)).toBeNull();
      expect(
        (
          screen.getByRole("button", {
            name: "Cancel recovery work",
          }) as HTMLButtonElement
        ).disabled,
      ).toBe(false);
    },
  );

  it("does not turn a cancellation acknowledgment or an unrelated idle result into completion", async () => {
    const actions = commands();
    const onActiveChange = vi.fn();
    await act(async () => {
      render(
        <RecoveryWorkControls
          actions={actions}
          watch={false}
          onActiveChange={onActiveChange}
        />,
      );
    });
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Cancel recovery work" }),
      );
    });
    expect(onActiveChange).toHaveBeenLastCalledWith(true);
    expect(screen.getByText(/work is still active/)).toBeTruthy();
    actions.status.mockResolvedValue({
      ...idle,
      recent: [{ ...job, id: "unrelated-job", state: "completed" }],
    });
    await tick();
    expect(
      screen.getByText(
        /terminal outcome for the last observed request is not yet available/,
      ),
    ).toBeTruthy();
    expect(screen.queryByText(/Last request: completed/)).toBeNull();
    expect(screen.getByText(/safe boundary before it stops/)).toBeTruthy();
    expect(onActiveChange).toHaveBeenLastCalledWith(false);
  });

  it.each(["completed", "cancelled", "failed"] as const)(
    "reports %s only when native terminal history identifies the same request",
    async (state) => {
      const actions = commands();
      await act(async () => {
        render(<RecoveryWorkControls actions={actions} watch={false} />);
      });
      await act(async () => {
        fireEvent.click(
          screen.getByRole("button", { name: "Cancel recovery work" }),
        );
      });
      actions.status.mockResolvedValue({
        ...idle,
        recent: [{ ...job, state, message: `Native result: ${state}` }],
      });
      await tick();
      expect(
        screen.getByText(new RegExp(`Last request: ${state}`)),
      ).toBeTruthy();
      expect(screen.getByText(`Native result: ${state}`)).toBeTruthy();
      expect(screen.queryByText(/safe boundary before it stops/)).toBeNull();
      expect(
        screen.queryByRole("button", { name: "Cancel recovery work" }),
      ).toBeNull();
    },
  );

  it("ignores a status response admitted before cancellation and keeps the cancel error separate", async () => {
    const actions = commands();
    const stale = deferred<RecoveryWorkStatus>();
    await act(async () => {
      render(<RecoveryWorkControls actions={actions} watch={false} />);
    });
    actions.status.mockReturnValueOnce(stale.promise);
    fireEvent.click(
      screen.getByRole("button", { name: "Refresh recovery work status" }),
    );
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Cancel recovery work" }),
      );
    });
    await act(async () => {
      stale.resolve({ ...idle, recent: [{ ...job, state: "completed" }] });
    });
    expect(screen.getByText(/work is still active/)).toBeTruthy();
    expect(screen.queryByText(/Last request: completed/)).toBeNull();
    await tick();
    expect(actions.status).toHaveBeenCalledTimes(3);
  });

  it("recovers from a failed status probe without clearing the independent cancellation failure", async () => {
    const actions = commands();
    const onActiveChange = vi.fn();
    await act(async () => {
      render(
        <RecoveryWorkControls
          actions={actions}
          watch={false}
          onActiveChange={onActiveChange}
        />,
      );
    });
    actions.cancel.mockRejectedValue(new Error("Cancel transport failed"));
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Cancel recovery work" }),
      );
    });
    actions.status.mockRejectedValue(new Error("Status transport failed"));
    await tick();
    expect(screen.getAllByRole("alert")).toHaveLength(2);
    expect(onActiveChange).toHaveBeenLastCalledWith(true);
    const calls = actions.status.mock.calls.length;
    await tick(10000);
    expect(actions.status).toHaveBeenCalledTimes(calls);
    actions.status.mockResolvedValue(idle);
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Refresh recovery work status" }),
      );
    });
    expect(screen.getByRole("alert").textContent).toContain(
      "Cancel transport failed",
    );
    expect(screen.queryByText(/Status transport failed/)).toBeNull();
    expect(onActiveChange).toHaveBeenLastCalledWith(false);
  });

  it("keeps unknown native status blocking external actions and retries visibly", async () => {
    const actions = commands();
    const onActiveChange = vi.fn();
    actions.status.mockRejectedValue(new Error("RECOVERY_STATUS_UNAVAILABLE"));
    await act(async () => {
      render(
        <RecoveryWorkControls
          actions={actions}
          watch={false}
          onActiveChange={onActiveChange}
        />,
      );
    });
    expect(onActiveChange).toHaveBeenLastCalledWith(true);
    expect(screen.getByRole("alert").textContent).toContain(
      "RECOVERY_STATUS_UNAVAILABLE",
    );
    await tick(10000);
    expect(actions.status).toHaveBeenCalledTimes(1);
    actions.status.mockResolvedValue(idle);
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Refresh recovery work status" }),
      );
    });
    expect(onActiveChange).toHaveBeenLastCalledWith(false);
  });

  it("polls at one-second intervals without overlap and stops after 120 automatic reads", async () => {
    const actions = commands();
    const initial = deferred<RecoveryWorkStatus>();
    actions.status.mockReturnValueOnce(initial.promise);
    render(<RecoveryWorkControls actions={actions} watch={true} />);
    await tick(10000);
    expect(actions.status).toHaveBeenCalledTimes(1);
    await act(async () => {
      initial.resolve(active);
    });
    await tick(999);
    expect(actions.status).toHaveBeenCalledTimes(1);
    await tick(1);
    expect(actions.status).toHaveBeenCalledTimes(2);
    for (let count = 1; count < 120; count++) await tick();
    expect(actions.status).toHaveBeenCalledTimes(121);
    expect(screen.getByText(/Automatic status checks are paused/)).toBeTruthy();
    await tick(10000);
    expect(actions.status).toHaveBeenCalledTimes(121);
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Refresh recovery work status" }),
      );
    });
    expect(actions.status).toHaveBeenCalledTimes(122);
    await tick();
    expect(actions.status).toHaveBeenCalledTimes(123);
  });

  it("ignores a prior action source's in-flight status and then reads the new source", async () => {
    const oldActions = commands(),
      nextActions = commands();
    const first = deferred<RecoveryWorkStatus>();
    oldActions.status.mockReturnValue(first.promise);
    nextActions.status.mockResolvedValue({
      ...active,
      active: { ...job, id: "next-job", phase: "current profile cleanup" },
    });
    const view = render(
      <RecoveryWorkControls actions={oldActions} watch={false} />,
    );
    view.rerender(<RecoveryWorkControls actions={nextActions} watch={false} />);
    await act(async () => {
      first.resolve(active);
    });
    expect(screen.queryByText(/walking staged files/)).toBeNull();
    await tick();
    expect(nextActions.status).toHaveBeenCalledOnce();
    expect(screen.getByText(/current profile cleanup/)).toBeTruthy();
  });

  it("releases parent busy state on unmount and ignores late status and cancel responses", async () => {
    const actions = commands(),
      onActiveChange = vi.fn();
    const cancel = deferred<{ id: string; cancelRequested: true }>();
    actions.cancel.mockReturnValue(cancel.promise);
    const view = render(
      <RecoveryWorkControls
        actions={actions}
        watch={false}
        onActiveChange={onActiveChange}
      />,
    );
    await act(async () => {});
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel recovery work" }),
    );
    view.unmount();
    expect(onActiveChange).toHaveBeenLastCalledWith(false);
    const calls = onActiveChange.mock.calls.length;
    await act(async () => {
      cancel.resolve({ id: job.id, cancelRequested: true });
    });
    await tick(10000);
    expect(onActiveChange).toHaveBeenCalledTimes(calls);
    expect(actions.status).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("ignores a late initial status after unmount without retaining parent busy state", async () => {
    const actions = commands(),
      onActiveChange = vi.fn();
    const status = deferred<RecoveryWorkStatus>();
    actions.status.mockReturnValue(status.promise);
    const view = render(
      <RecoveryWorkControls
        actions={actions}
        watch={true}
        onActiveChange={onActiveChange}
      />,
    );
    expect(onActiveChange).toHaveBeenLastCalledWith(true);
    view.unmount();
    const calls = onActiveChange.mock.calls.length;
    await act(async () => {
      status.resolve(active);
    });
    await tick(10000);
    expect(onActiveChange).toHaveBeenLastCalledWith(false);
    expect(onActiveChange).toHaveBeenCalledTimes(calls);
    expect(actions.status).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("retires a prior action source's pending cancel without applying its acknowledgment", async () => {
    const oldActions = commands(),
      nextActions = commands();
    const cancellation = deferred<{ id: string; cancelRequested: true }>();
    oldActions.cancel.mockReturnValue(cancellation.promise);
    const view = render(
      <RecoveryWorkControls actions={oldActions} watch={false} />,
    );
    await act(async () => {});
    oldActions.status.mockRejectedValue(new Error("old status failed"));
    await tick();
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel recovery work" }),
    );
    nextActions.status.mockResolvedValue({
      ...active,
      active: { ...job, id: "new-source-job", phase: "new source work" },
    });
    view.rerender(<RecoveryWorkControls actions={nextActions} watch={false} />);
    await act(async () => {
      cancellation.resolve({ id: job.id, cancelRequested: true });
    });
    expect(screen.queryByText(/safe boundary before it stops/)).toBeNull();
    await tick();
    expect(nextActions.status).toHaveBeenCalledOnce();
    expect(screen.getByText(/new source work/)).toBeTruthy();
    expect(
      (
        screen.getByRole("button", {
          name: "Cancel recovery work",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(false);
  });
});
