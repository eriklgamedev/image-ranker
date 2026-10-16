// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { StartupLoading } from "../../src/components/StartupLoading";
import { RecoveryWorkControls } from "../../src/components/RecoveryWorkControls";
import type { RecoveryWorkJob, RecoveryWorkStatus } from "../../src/native";
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});
const job: RecoveryWorkJob = {
  id: "native-profile-work",
  contextId: "old-renderer",
  profileGeneration: "profile",
  kind: "prepare-profile",
  phase: "opening-profile",
  state: "running",
  completed: 0,
  total: null,
  cancelRequested: false,
  message: null,
};
const idle: RecoveryWorkStatus = { active: null, recent: [], limit: 1 };
const flush = () => act(async () => {});
it.each([
  [
    "startup-load",
    "loading-session",
    "Loading local workspace",
    "Loading the saved session",
    false,
  ],
  [
    "startup-load",
    "loading-operation-history",
    "Loading local workspace",
    "Loading file operation history",
    false,
  ],
  [
    "prepare-profile",
    "opening-profile",
    "Preparing profile storage",
    "Opening profile storage",
    false,
  ],
  [
    "prepare-profile",
    "published",
    "Preparing profile storage",
    "Finishing the selected profile",
    true,
  ],
  [
    "switch-profile",
    "checking-current-save",
    "Switching profile",
    "Checking the current session save",
    false,
  ],
  [
    "switch-profile",
    "committing-selection",
    "Switching profile",
    "Committing profile selection",
    true,
  ],
  [
    "switch-profile",
    "publishing",
    "Switching profile",
    "Publishing the selected profile",
    true,
  ],
  [
    "switch-profile",
    "published",
    "Switching profile",
    "Finishing the selected profile",
    true,
  ],
] as const)(
  "labels %s %s and obeys native seal",
  async (kind, phase, title, label, sealed) => {
    const actions = {
      status: vi
        .fn()
        .mockResolvedValue({ ...idle, active: { ...job, kind, phase } }),
      cancel: vi.fn(),
    };
    render(<RecoveryWorkControls actions={actions} watch={false} />);
    await flush();
    expect(screen.getByText(new RegExp(`${title}.*${label}`))).toBeTruthy();
    expect(
      screen
        .getByRole("button", { name: "Cancel recovery work" })
        .hasAttribute("disabled"),
    ).toBe(sealed);
    expect(screen.queryByText(/0 of/)).toBeNull();
  },
);
it("retains a retired profile job through finishing rejection and absent terminal status", async () => {
  const actions = {
    status: vi.fn().mockResolvedValue({ ...idle, active: job }),
    cancel: vi
      .fn()
      .mockRejectedValue(
        new Error("RECOVERY_JOB_FINISHING: selection is sealed"),
      ),
  };
  const busy = vi.fn();
  render(
    <RecoveryWorkControls
      actions={actions}
      watch={false}
      onActiveChange={busy}
    />,
  );
  await flush();
  fireEvent.click(screen.getByRole("button", { name: "Cancel recovery work" }));
  await flush();
  expect(actions.cancel).toHaveBeenCalledWith(job.id);
  expect(screen.queryByText(/^Cancellation requested\./)).toBeNull();
  actions.status.mockResolvedValue(idle);
  fireEvent.click(
    screen.getByRole("button", { name: "Refresh recovery work status" }),
  );
  await flush();
  expect(busy).toHaveBeenLastCalledWith(true);
  expect(screen.getByText(/Native activation is finishing/)).toBeTruthy();
  actions.status.mockResolvedValue({
    ...idle,
    recent: [{ ...job, state: "completed" }],
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Refresh recovery work status" }),
  );
  await flush();
  expect(busy).toHaveBeenLastCalledWith(false);
  expect(screen.getByText(/Last request: completed/)).toBeTruthy();
  expect(screen.getByRole("alert").textContent).toContain(
    "RECOVERY_JOB_FINISHING",
  );
});
it("retains a cancellation error when its owning command settles during the cancel acknowledgment", async () => {
  let reject!: (reason: unknown) => void;
  const pending = new Promise<{ id: string; cancelRequested: true }>(
    (_, fail) => {
      reject = fail;
    },
  );
  const actions = {
    status: vi.fn().mockResolvedValue({ ...idle, active: job }),
    cancel: vi.fn().mockReturnValue(pending),
  };
  const view = render(
    <RecoveryWorkControls actions={actions} watch={true} refreshKey={0} />,
  );
  await flush();
  fireEvent.click(screen.getByRole("button", { name: "Cancel recovery work" }));
  view.rerender(
    <RecoveryWorkControls actions={actions} watch={false} refreshKey={1} />,
  );
  await act(async () =>
    reject(new Error("RECOVERY_JOB_FINISHING: same selection is finishing")),
  );
  expect(screen.getByRole("alert").textContent).toContain(
    "RECOVERY_JOB_FINISHING",
  );
  expect(screen.queryByText(/^Cancellation requested\./)).toBeNull();
});
it("bounds startup polling to 120 nonoverlapping reads and permits manual refresh without restarting", async () => {
  vi.useFakeTimers();
  let resolve!: () => void;
  const first = new Promise<void>((done) => {
    resolve = done;
  });
  const refresh = vi
    .fn()
    .mockReturnValueOnce(first)
    .mockResolvedValue(undefined);
  const help = vi.fn();
  const view = render(
    <StartupLoading watch={true} onRefresh={refresh} onHelp={help} />,
  );
  await act(async () => {
    await vi.advanceTimersByTimeAsync(1000);
  });
  expect(refresh).toHaveBeenCalledTimes(1);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10000);
  });
  expect(refresh).toHaveBeenCalledTimes(1);
  fireEvent.click(
    screen.getByRole("button", { name: "Help and local diagnostics" }),
  );
  expect(help).toHaveBeenCalledOnce();
  await act(async () => resolve());
  for (let index = 1; index < 120; index++)
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
  expect(refresh).toHaveBeenCalledTimes(120);
  expect(
    screen.getByText(/Automatic workspace checks are paused/),
  ).toBeTruthy();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10000);
  });
  expect(refresh).toHaveBeenCalledTimes(120);
  fireEvent.click(
    screen.getByRole("button", { name: "Refresh workspace status" }),
  );
  await flush();
  expect(refresh).toHaveBeenCalledTimes(121);
  view.unmount();
  expect(vi.getTimerCount()).toBe(0);
});
it("stops startup observation after a failed read and ignores late unmounted resolution", async () => {
  vi.useFakeTimers();
  const refresh = vi.fn().mockRejectedValue(new Error("STATUS_BUSY"));
  const view = render(
    <StartupLoading watch={true} onRefresh={refresh} onHelp={() => {}} />,
  );
  await act(async () => {
    await vi.advanceTimersByTimeAsync(1000);
  });
  expect(screen.getByRole("alert").textContent).toContain("STATUS_BUSY");
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10000);
  });
  expect(refresh).toHaveBeenCalledOnce();
  let finish!: () => void;
  refresh.mockImplementation(
    () =>
      new Promise<void>((done) => {
        finish = done;
      }),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Refresh workspace status" }),
  );
  view.unmount();
  await act(async () => finish());
  expect(vi.getTimerCount()).toBe(0);
});
