// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { RecoveryWorkControls } from "../../src/components/RecoveryWorkControls";
import { StartupRecovery } from "../../src/components/StartupRecovery";
import { HelpDialog } from "../../src/components/HelpDialog";
import type { RecoveryWorkJob, RecoveryWorkStatus } from "../../src/native";
const native = vi.hoisted(() => ({
  appInfo: vi.fn(),
  licenseNotices: vi.fn(),
  storageLocations: vi.fn(),
}));
vi.mock("../../src/native", () => ({ desktop: true, api: native }));
vi.mock("../../src/components/ImageWorkSettings", () => ({
  ImageWorkSettings: () => null,
}));
vi.mock("../../src/components/ProtectionStatusPanel", () => ({
  ProtectionStatusPanel: () => null,
}));
const job: RecoveryWorkJob = {
  id: "inspect-A",
  contextId: "retired-context",
  profileGeneration: "profile-A",
  kind: "inspect-copy",
  phase: "awaiting-picker",
  state: "running",
  completed: 0,
  total: null,
  cancelRequested: false,
  message: null,
};
const idle: RecoveryWorkStatus = { active: null, recent: [], limit: 1 };
const active = { ...idle, active: job };
const commands = () => ({
  status: vi.fn().mockResolvedValue(active),
  cancel: vi.fn().mockResolvedValue({ id: job.id, cancelRequested: true }),
});
function deferred<T>() {
  let resolve!: (value: T) => void, reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
async function tick() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(1000);
  });
}
beforeEach(() => {
  vi.useFakeTimers();
  vi.resetAllMocks();
  native.appInfo.mockReturnValue(new Promise(() => {}));
  native.licenseNotices.mockResolvedValue([]);
  native.storageLocations.mockResolvedValue([]);
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});
it("names fresh inspection and explains native picker ownership before cancellation", async () => {
  const actions = commands();
  await act(async () => {
    render(<RecoveryWorkControls actions={actions} watch={false} />);
  });
  expect(
    screen.getByText(/Inspecting recovery copy · Waiting for the file picker/),
  ).toBeTruthy();
  expect(
    screen.getByText(/A cancellation request cannot dismiss that window/),
  ).toBeTruthy();
});

it.each([
  ["reading-copy", "Reading recovery copy"],
  ["decoding-copy", "Decoding recovery copy"],
  ["validating-replay", "Validating retained history"],
  ["publishing", "Publishing validated inspection"],
])(
  "shows the native inspection phase %s without inferring completion",
  async (phase, label) => {
    const actions = commands();
    actions.status.mockResolvedValue({
      ...active,
      active: {
        ...job,
        phase,
        completed: phase === "reading-copy" ? 4096 : 0,
        total: phase === "reading-copy" ? 8192 : null,
      },
    });
    await act(async () => {
      render(<RecoveryWorkControls actions={actions} watch={false} />);
    });
    expect(
      screen.getByText(new RegExp(`Inspecting recovery copy · ${label}`)),
    ).toBeTruthy();
    if (phase === "reading-copy")
      expect(screen.getByText(/4096 of 8192 bytes/)).toBeTruthy();
    if (["decoding-copy", "validating-replay"].includes(phase))
      expect(
        screen.getByText(
          /Cancellation is checked at bounded validation boundaries/,
        ),
      ).toBeTruthy();
    expect(screen.queryByText(/Last request: completed/)).toBeNull();
  },
);
it("keeps inspection active after cancel ACK until both native terminal state and the pending command settle", async () => {
  const actions = commands();
  actions.status.mockResolvedValue(idle);
  const operation = deferred<null>();
  const onOpen = vi.fn(() => operation.promise),
    onBusyChange = vi.fn();
  await act(async () => {
    render(
      <StartupRecovery
        status={{ mode: "recovery", issues: [], canRestoreBackup: true }}
        failure=""
        onRetry={async () => {}}
        onOpen={onOpen}
        onRestored={() => {}}
        onBusyChange={onBusyChange}
        work={actions}
      />,
    );
  });
  actions.status.mockResolvedValue(active);
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Open recovery copy" }));
  });
  expect(onOpen).toHaveBeenCalledOnce();
  await act(async () => {
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel recovery work" }),
    );
  });
  expect(actions.cancel).toHaveBeenCalledExactlyOnceWith(job.id);
  expect(
    screen.getByText(/Cancellation requested; work is still active/),
  ).toBeTruthy();
  expect(onBusyChange).toHaveBeenLastCalledWith(true);
  actions.status.mockResolvedValue({
    ...idle,
    recent: [{ ...job, state: "cancelled", cancelRequested: true }],
  });
  await tick();
  expect(
    screen
      .getByRole("button", { name: "Open recovery copy" })
      .hasAttribute("disabled"),
  ).toBe(true);
  expect(onBusyChange).toHaveBeenLastCalledWith(true);
  await act(async () => {
    operation.reject(new Error("RECOVERY_CANCELLED: safe boundary reached"));
  });
  expect(
    screen
      .getByRole("button", { name: "Open recovery copy" })
      .hasAttribute("disabled"),
  ).toBe(false);
  expect(onBusyChange).toHaveBeenLastCalledWith(false);
  expect(screen.queryByText("Recovery copy preview")).toBeNull();
});
it("does not announce cancellation when native publication is already finishing", async () => {
  const actions = commands();
  actions.cancel.mockRejectedValue(
    new Error("RECOVERY_JOB_FINISHING: wait for the native outcome"),
  );
  await act(async () => {
    render(<RecoveryWorkControls actions={actions} watch={false} />);
  });
  await act(async () => {
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel recovery work" }),
    );
  });
  expect(screen.getByRole("alert").textContent).toContain(
    "RECOVERY_JOB_FINISHING",
  );
  expect(
    screen.queryByText(/Cancellation requested; work is still active/),
  ).toBeNull();
  actions.status.mockResolvedValue({
    ...idle,
    recent: [{ ...job, state: "completed", phase: "publishing" }],
  });
  await tick();
  expect(screen.getByText(/Last request: completed/)).toBeTruthy();
  expect(screen.getByRole("alert").textContent).toContain(
    "RECOVERY_JOB_FINISHING",
  );
});
it.each(["cancelled", "failed", "picker-cancelled"])(
  "preserves the prior inspected subject and imported choice when another inspection is %s",
  async (outcome) => {
    const actions = commands();
    actions.status.mockResolvedValue(idle);
    const original = {
      token: "prior-subject",
      createdAt: "2026-09-29",
      summary: { images: 3, runs: 1, decisions: 2, reviewEdits: 0 },
      warnings: [],
      restorable: true,
      pendingActions: 1,
      pendingSummary: ["Imported vote"],
    };
    const onOpen = vi.fn().mockResolvedValueOnce(original);
    if (outcome === "picker-cancelled") onOpen.mockResolvedValueOnce(null);
    else
      onOpen.mockRejectedValueOnce(
        new Error(
          outcome === "cancelled"
            ? "RECOVERY_CANCELLED: stopped reading"
            : "RECOVERY_FORMAT: bad copy",
        ),
      );
    const onRestore = vi
      .fn()
      .mockResolvedValue({ mode: "ready", issues: [], canRestoreBackup: true });
    await act(async () => {
      render(
        <StartupRecovery
          status={{ mode: "recovery", issues: [], canRestoreBackup: true }}
          failure=""
          onRetry={async () => {}}
          onOpen={onOpen}
          onRestore={onRestore}
          onRestored={() => {}}
          work={actions}
        />,
      );
    });
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Open recovery copy" }),
      );
    });
    fireEvent.click(
      screen.getByLabelText(
        "Restore the saved state and apply the displayed pending action once after validation.",
      ),
    );
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Inspect another recovery copy" }),
      );
    });
    expect(screen.getByText("Recovery copy preview")).toBeTruthy();
    expect((screen.getByRole("checkbox") as HTMLInputElement).checked).toBe(
      true,
    );
    expect(onRestore).not.toHaveBeenCalled();
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Restore as a new session" }),
      );
    });
    expect(onRestore).toHaveBeenCalledExactlyOnceWith("prior-subject", true);
  },
);
it.each(["unresolved", "rejected"])(
  "keeps Help status and retired-job cancellation independent of %s support metadata",
  async (state) => {
    if (state === "rejected")
      native.appInfo.mockRejectedValue(
        new Error("SUPPORT_BUSY: retry metadata"),
      );
    const actions = commands();
    await act(async () => {
      render(<HelpDialog onClose={() => {}} work={actions} />);
    });
    expect(screen.getByText(/Inspecting recovery copy/)).toBeTruthy();
    if (state === "unresolved")
      expect(
        screen.getByText(/Loading local support information/),
      ).toBeTruthy();
    else
      expect(
        screen.getByText(/Application details: Error: SUPPORT_BUSY/),
      ).toBeTruthy();
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Cancel recovery work" }),
      );
    });
    expect(actions.cancel).toHaveBeenCalledExactlyOnceWith("inspect-A");
    expect(
      screen.getByText(/Cancellation requested; work is still active/),
    ).toBeTruthy();
    actions.status.mockResolvedValue({
      ...idle,
      recent: [{ ...job, state: "cancelled", cancelRequested: true }],
    });
    await tick();
    expect(screen.getByText(/Last request: cancelled/)).toBeTruthy();
  },
);
it("rediscovers a retired-context inspection on Help remount and cancels only its native ID", async () => {
  const actions = commands();
  let view!: ReturnType<typeof render>;
  await act(async () => {
    view = render(<HelpDialog onClose={() => {}} work={actions} />);
  });
  view.unmount();
  await act(async () => {
    render(<HelpDialog onClose={() => {}} work={actions} />);
  });
  expect(actions.status).toHaveBeenCalledTimes(2);
  await act(async () => {
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel recovery work" }),
    );
  });
  expect(actions.cancel).toHaveBeenCalledExactlyOnceWith(job.id);
  expect(screen.queryByText(/Last request: cancelled/)).toBeNull();
});
