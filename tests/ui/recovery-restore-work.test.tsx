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
import { RecoveryReplacement } from "../../src/components/RecoveryReplacement";
import type {
  BootStatus,
  RecoveryReplacementPreview,
  RecoveryReplacementSubject,
  RecoveryWorkJob,
  RecoveryWorkStatus,
} from "../../src/native";

const job: RecoveryWorkJob = {
  id: "restore-worker-A",
  contextId: "retired-renderer",
  profileGeneration: "profile-A",
  kind: "restore-copy",
  phase: "prepare-activation",
  state: "running",
  completed: 0,
  total: null,
  cancelRequested: false,
  message: null,
};
const idle: RecoveryWorkStatus = { active: null, recent: [], limit: 1 };
const active: RecoveryWorkStatus = { ...idle, active: job };
const actionsFor = (current = active) => ({
  status: vi.fn().mockResolvedValue(current),
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
beforeEach(() => vi.useFakeTimers());
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

it("labels an active restore without announcing completion", async () => {
  await act(async () => {
    render(<RecoveryWorkControls actions={actionsFor()} watch={false} />);
  });
  expect(
    screen.getByText(/Restoring recovery copy · Preparing activation/),
  ).toBeTruthy();
  expect(
    screen.getByText(
      /Cancellation requests a safe stop before activation begins/,
    ),
  ).toBeTruthy();
  expect(screen.queryByText(/Last request: completed/)).toBeNull();
});

it.each([
  ["checking-current-save", "Checking the current session save"],
  ["checkpoint-current", "Checkpointing the current session"],
  ["staging", "Staging the recovered session"],
  ["bootstrap", "Building the recovered session history"],
  ["prepare-activation", "Preparing activation"],
  ["committing", "Committing activation"],
  ["publishing", "Publishing the selected session"],
])(
  "renders phase-only native restore progress for %s",
  async (phase, label) => {
    const actions = actionsFor({ ...active, active: { ...job, phase } });
    await act(async () => {
      render(<RecoveryWorkControls actions={actions} watch={false} />);
    });
    expect(screen.getByText(`Restoring recovery copy · ${label}`)).toBeTruthy();
    expect(screen.queryByText(/\d+ of \d+/)).toBeNull();
    expect(screen.queryByText(/Last request: completed/)).toBeNull();
    const sealed = ["committing", "publishing"].includes(phase);
    expect(
      (
        screen.getByRole("button", {
          name: "Cancel recovery work",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(sealed);
    if (sealed)
      expect(
        screen.getByText(/Cancellation can no longer stop this selection/),
      ).toBeTruthy();
  },
);

it("keeps an acknowledged safe-stop request active until native reports its terminal result", async () => {
  const actions = actionsFor(),
    busy = vi.fn();
  await act(async () => {
    render(
      <RecoveryWorkControls
        actions={actions}
        watch={false}
        onActiveChange={busy}
      />,
    );
  });
  await act(async () => {
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel recovery work" }),
    );
  });
  expect(actions.cancel).toHaveBeenCalledExactlyOnceWith(job.id);
  expect(
    screen.getByText(/Cancellation requested; work is still active/),
  ).toBeTruthy();
  expect(busy).toHaveBeenLastCalledWith(true);
  await tick();
  expect(screen.queryByText(/Last request: cancelled/)).toBeNull();
  actions.status.mockResolvedValue({
    ...idle,
    recent: [{ ...job, state: "cancelled", cancelRequested: true }],
  });
  await tick();
  expect(screen.getByText(/Last request: cancelled/)).toBeTruthy();
  expect(busy).toHaveBeenLastCalledWith(false);
});

it("keeps a published restore sealed until native finishes releasing its retained resources", async () => {
  const actions = actionsFor({
    ...active,
    active: { ...job, phase: "published" },
  });
  const busy = vi.fn();
  await act(async () => {
    render(
      <RecoveryWorkControls
        actions={actions}
        watch={false}
        onActiveChange={busy}
      />,
    );
  });
  expect(
    screen.getByText(
      /Restoring recovery copy · Finishing the selected session/,
    ),
  ).toBeTruthy();
  expect(
    (
      screen.getByRole("button", {
        name: "Cancel recovery work",
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  expect(screen.queryByText(/Cancellation requests a safe stop/)).toBeNull();
  expect(
    screen.getByText(/Cancellation can no longer stop this selection/),
  ).toBeTruthy();
  expect(busy).toHaveBeenLastCalledWith(true);
  actions.status.mockResolvedValue(idle);
  await tick();
  expect(screen.queryByText(/No recovery work is active/)).toBeNull();
  expect(busy).toHaveBeenLastCalledWith(true);
  actions.status.mockResolvedValue({
    ...idle,
    recent: [{ ...job, phase: "published", state: "completed" }],
  });
  await tick();
  expect(screen.getByText(/Last request: completed/)).toBeTruthy();
  expect(busy).toHaveBeenLastCalledWith(false);
  expect(actions.cancel).not.toHaveBeenCalled();
});

it("treats finishing rejection as sealed activation and never as cancellation or unknown idle", async () => {
  const actions = actionsFor(),
    busy = vi.fn();
  actions.cancel.mockRejectedValue(
    new Error("RECOVERY_JOB_FINISHING: activation is finishing"),
  );
  await act(async () => {
    render(
      <RecoveryWorkControls
        actions={actions}
        watch={false}
        onActiveChange={busy}
      />,
    );
  });
  await act(async () => {
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel recovery work" }),
    );
  });
  expect(
    screen.getByText(
      /Activation is finishing. Cancellation can no longer stop/,
    ),
  ).toBeTruthy();
  expect(
    screen.queryByText(/Cancellation requested; work is still active/),
  ).toBeNull();
  expect(screen.getByRole("alert").textContent).toContain(
    "RECOVERY_JOB_FINISHING",
  );
  expect(
    (
      screen.getByRole("button", {
        name: "Cancel recovery work",
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  actions.status.mockResolvedValue(idle);
  await tick();
  expect(
    screen.getByText(
      /Native activation is finishing; its terminal outcome is not yet available/,
    ),
  ).toBeTruthy();
  expect(screen.queryByText(/No recovery work is active/)).toBeNull();
  expect(busy).toHaveBeenLastCalledWith(true);
  // A successful status probe cannot erase the cancellation rejection or create
  // a terminal result for this exact job. Polling remains bounded/observational.
  actions.status.mockResolvedValue({
    ...idle,
    recent: [{ ...job, id: "unrelated", state: "completed" }],
  });
  await tick();
  expect(screen.queryByText(/Last request: completed/)).toBeNull();
  expect(screen.getByRole("alert").textContent).toContain(
    "RECOVERY_JOB_FINISHING",
  );
  actions.status.mockResolvedValue({
    ...idle,
    recent: [{ ...job, state: "completed" }],
  });
  await tick();
  expect(screen.getByText(/Last request: completed/)).toBeTruthy();
  expect(busy).toHaveBeenLastCalledWith(false);
});

it("finds a retired-context restore after remount and preserves its sealed outcome wait", async () => {
  const actions = actionsFor({
    ...active,
    active: { ...job, phase: "committing" },
  });
  const first = render(
    <RecoveryWorkControls actions={actions} watch={false} />,
  );
  await act(async () => {});
  first.unmount();
  const busy = vi.fn();
  await act(async () => {
    render(
      <RecoveryWorkControls
        actions={actions}
        watch={false}
        onActiveChange={busy}
      />,
    );
  });
  expect(
    screen.getByText(/Restoring recovery copy · Committing activation/),
  ).toBeTruthy();
  expect(actions.cancel).not.toHaveBeenCalled();
  actions.status.mockResolvedValue(idle);
  await tick();
  expect(busy).toHaveBeenLastCalledWith(true);
  actions.status.mockResolvedValue({
    ...idle,
    recent: [
      {
        ...job,
        state: "failed",
        message:
          "ACTIVATION_RECOVERY_REQUIRED: exact request remains uncertain",
      },
    ],
  });
  await tick();
  expect(screen.getByText(/Last request: failed/)).toBeTruthy();
  expect(screen.getByText(/ACTIVATION_RECOVERY_REQUIRED/)).toBeTruthy();
});

it("ignores a late finishing rejection after its action source is replaced", async () => {
  const actions = actionsFor(),
    next = actionsFor(idle);
  const cancel = deferred<{ id: string; cancelRequested: true }>();
  actions.cancel.mockReturnValue(cancel.promise);
  const view = render(<RecoveryWorkControls actions={actions} watch={false} />);
  await act(async () => {});
  fireEvent.click(screen.getByRole("button", { name: "Cancel recovery work" }));
  view.rerender(<RecoveryWorkControls actions={next} watch={false} />);
  await act(async () => {
    cancel.reject(new Error("RECOVERY_JOB_FINISHING: retired result"));
  });
  await tick();
  expect(screen.getByText(/No recovery work is active/)).toBeTruthy();
  expect(screen.queryByText(/Native activation is finishing/)).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
});

const subject: RecoveryReplacementSubject = {
  kind: "fresh",
  token: "inspected-original",
  acceptPending: true,
};
const preview: RecoveryReplacementPreview = {
  token: "replacement-exact",
  subjectToken: subject.token,
  subjectKind: "fresh",
  currentSaveState: "pending",
  hasCurrentSession: true,
  requiresDisposition: true,
  rendererChangesUnadmitted: false,
  warnings: [],
  pendingLayers: [
    {
      kind: "pending-event",
      id: "current-event",
      summary: "Current last vote has no durable acknowledgment",
    },
    {
      kind: "pending-proposal",
      id: "current-proposal",
      summary: "Current later pick edits remain in memory",
    },
  ],
};
const boot: BootStatus = {
  mode: "ready",
  state: "ready",
  issues: [],
  canRestoreBackup: true,
  pendingOperations: 0,
  profile: {
    id: "profile-A",
    displayPath: "C:/fixture/profile",
    isDefault: true,
    writable: true,
  },
  profileGeneration: "profile-A",
  contextId: "restored-context",
  canChooseRecoveryStorage: true,
};
it("does not promise a saved-only import when imported pending actions were not approved", async () => {
  const noConsent: RecoveryReplacementSubject = {
    ...subject,
    acceptPending: false,
  };
  const commands = {
    prepare: vi
      .fn()
      .mockRejectedValue(
        new Error(
          "PENDING_CONFIRMATION_REQUIRED: imported pending actions need approval",
        ),
      ),
    cancel: vi.fn(),
    exportCopy: vi.fn(),
    confirm: vi.fn(),
  };
  await act(async () => {
    render(
      <RecoveryReplacement
        subject={noConsent}
        generation={1}
        commands={commands}
        canRetrySave={false}
        flush={async () => {}}
        retry={async () => {}}
        snapshot={() => undefined}
        onClose={() => {}}
        onRestored={() => {}}
      />,
    );
  });
  expect(
    screen.getByText(
      "No imported pending actions have been approved for this restore.",
    ),
  ).toBeTruthy();
  expect(screen.queryByText(/Restore only the saved state/)).toBeNull();
  expect(commands.confirm).not.toHaveBeenCalled();
});
async function replacementSetup() {
  const result = deferred<BootStatus>();
  const actions = actionsFor(idle);
  const commands = {
    prepare: vi.fn().mockResolvedValue(preview),
    cancel: vi.fn().mockResolvedValue(undefined),
    exportCopy: vi.fn().mockResolvedValue("C:/fixture/current-recovery.json"),
    confirm: vi.fn().mockReturnValue(result.promise),
  };
  const props = {
    subject,
    generation: 1,
    commands,
    work: actions,
    canRetrySave: true,
    flush: vi.fn().mockRejectedValue(new Error("current append still failed")),
    retry: vi.fn(),
    snapshot: () => ({ name: "actual-current" }),
    onClose: vi.fn(),
    onRestored: vi.fn(),
  };
  await act(async () => {
    render(<RecoveryReplacement {...props} />);
  });
  actions.status.mockResolvedValue(active);
  await act(async () => {
    fireEvent.click(
      screen.getByRole("button", {
        name: "Save current recovery copy and restore",
      }),
    );
  });
  return { props, commands, actions, result };
}

it("keeps current pending layers and imported consent readable during safe-stop and settles only the native result", async () => {
  const { props, commands, actions, result } = await replacementSetup();
  expect(
    screen.getByText(/previously approved imported pending actions/),
  ).toBeTruthy();
  expect(
    screen.getByText(/Current last vote has no durable acknowledgment/),
  ).toBeTruthy();
  expect(
    screen.getByText(/Current later pick edits remain in memory/),
  ).toBeTruthy();
  expect(commands.confirm).toHaveBeenCalledExactlyOnceWith(
    subject,
    preview.token,
    "recovery-copy",
  );
  await act(async () => {
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel recovery work" }),
    );
  });
  expect(actions.cancel).toHaveBeenCalledExactlyOnceWith(job.id);
  expect(
    (
      screen.getByRole("button", {
        name: "Keep current session",
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  expect(props.onRestored).not.toHaveBeenCalled();
  actions.status.mockResolvedValue({
    ...idle,
    recent: [{ ...job, state: "cancelled" }],
  });
  await tick();
  expect(
    (
      screen.getByRole("button", {
        name: "Keep current session",
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  await act(async () => {
    result.reject(new Error("RECOVERY_CANCELLED: safe pre-seal stop"));
  });
  expect(
    (
      screen.getByRole("button", {
        name: "Keep current session",
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(false);
  expect(
    screen.queryByRole("button", { name: "Check restore outcome" }),
  ).toBeNull();
  expect(
    screen.getByText(/Current last vote has no durable acknowledgment/),
  ).toBeTruthy();
  expect(commands.prepare).toHaveBeenCalledOnce();
  expect(props.onRestored).not.toHaveBeenCalled();
});

it.each([
  "IPC response lost",
  "ACTIVATION_RECOVERY_REQUIRED: commit must be reconciled",
])(
  "preserves exact confirmation identity after %s while restore work remains discoverable",
  async (message) => {
    const { props, commands, actions, result } = await replacementSetup();
    actions.cancel.mockRejectedValue(
      new Error("RECOVERY_JOB_FINISHING: activation is finishing"),
    );
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Cancel recovery work" }),
      );
    });
    await act(async () => {
      result.reject(new Error(message));
    });
    expect(
      screen.getByText(/Current later pick edits remain in memory/),
    ).toBeTruthy();
    expect(
      screen.getByText(/previously approved imported pending actions/),
    ).toBeTruthy();
    expect(
      (
        screen.getByRole("button", {
          name: "Keep current session",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
    expect(
      screen.queryByRole("button", {
        name: "Refresh replacement authorization",
      }),
    ).toBeNull();
    expect(commands.cancel).not.toHaveBeenCalled();
    actions.status.mockResolvedValue({
      ...idle,
      recent: [{ ...job, state: "completed" }],
    });
    await tick();
    expect(props.onRestored).not.toHaveBeenCalled();
    commands.confirm.mockResolvedValue(boot);
    await act(async () => {
      fireEvent.click(
        screen.getByRole("button", { name: "Check restore outcome" }),
      );
    });
    expect(commands.confirm.mock.calls).toEqual([
      [subject, preview.token, "recovery-copy"],
      [subject, preview.token, "recovery-copy"],
    ]);
    expect(commands.prepare).toHaveBeenCalledOnce();
    expect(commands.exportCopy).toHaveBeenCalledOnce();
    expect(props.onRestored).toHaveBeenCalledExactlyOnceWith(boot);
  },
);
