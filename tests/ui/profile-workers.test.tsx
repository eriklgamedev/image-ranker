// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ProfileSwitcher } from "../../src/components/ProfileSwitcher";
import type { RecoveryWorkJob, RecoveryWorkStatus } from "../../src/native";

afterEach(cleanup);
const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
};
const prepared = {
  token: "candidate-1",
  mode: "open" as const,
  profile: {
    id: "target",
    displayPath: "D:/Chosen",
    isDefault: false,
    writable: true,
  },
  hasPendingSave: false,
  pendingOperations: 0,
  requiresDisposition: false,
  rendererChangesUnadmitted: false,
  warnings: [],
};
const job = {
  id: "prepare-1",
  contextId: "context",
  profileGeneration: "profile",
  kind: "prepare-profile",
  phase: "awaiting-picker",
  state: "running",
  completed: 0,
  total: null,
  cancelRequested: false,
  message: null,
} as RecoveryWorkJob;
const idle = { active: null, recent: [], limit: 1 } as RecoveryWorkStatus;
function setup(
  options: {
    dirty?: boolean;
    unhydrated?: boolean;
    unadmitted?: boolean;
    held?: boolean;
  } = {},
) {
  const choice = deferred<typeof prepared | null>();
  const commands = {
    prepare: vi
      .fn()
      .mockImplementation(() =>
        options.held
          ? choice.promise
          : Promise.resolve({
              ...prepared,
              requiresDisposition: !!options.dirty,
              rendererChangesUnadmitted: !!options.unadmitted,
            }),
      ),
    refresh: vi.fn().mockResolvedValue({ ...prepared, token: "candidate-2" }),
    confirm: vi
      .fn()
      .mockResolvedValue({ mode: "ready", issues: [], canRestoreBackup: true }),
    cancel: vi.fn().mockResolvedValue(undefined),
    exportCopy: vi.fn().mockResolvedValue("copy.json"),
  };
  const work = {
    status: vi
      .fn()
      .mockResolvedValue(options.held ? { ...idle, active: job } : idle),
    cancel: vi.fn().mockResolvedValue({ id: job.id, cancelRequested: true }),
  };
  const snapshot = vi.fn(() =>
    options.unhydrated ? undefined : { pending: "current UI" },
  );
  const props = {
    mode: "open" as const,
    generation: 1,
    commands,
    work,
    flush: vi.fn().mockResolvedValue(undefined),
    retry: vi.fn().mockResolvedValue(undefined),
    snapshot,
    canRetrySave: !options.unhydrated,
    onClose: vi.fn(),
    onSwitched: vi.fn(),
    onHelp: vi.fn(),
  };
  render(<ProfileSwitcher {...props} />);
  return { ...props, choice };
}
async function approve() {
  const input = await screen.findByRole("checkbox", {
    name: "I reviewed this profile and its location.",
  });
  await waitFor(() => expect(input.hasAttribute("disabled")).toBe(false));
  fireEvent.click(input);
}
async function refresh() {
  fireEvent.click(
    screen.getByRole("button", { name: "Refresh recovery work status" }),
  );
  await act(async () => {});
}
it("keeps picker cancellation frozen until exact worker terminal and command settlement", async () => {
  const p = setup({ held: true });
  await screen.findByText(/Preparing profile storage/);
  fireEvent.click(screen.getByRole("button", { name: "Cancel recovery work" }));
  await screen.findByText(/Cancellation requested. Recovery work/);
  p.work.status.mockResolvedValue(idle);
  await refresh();
  await act(async () => p.choice.resolve(null));
  expect(p.onClose).not.toHaveBeenCalled();
  p.work.status.mockResolvedValue({
    ...idle,
    recent: [{ ...job, state: "cancelled" }],
  });
  await refresh();
  await waitFor(() => expect(p.onClose).toHaveBeenCalledTimes(1));
});
it("keeps Help accessible while the picker promise is unresolved", async () => {
  const p = setup({ held: true });
  fireEvent.click(
    await screen.findByRole("button", { name: "Help and diagnostics" }),
  );
  expect(p.onHelp).toHaveBeenCalledTimes(1);
  expect(p.commands.confirm).not.toHaveBeenCalled();
});
it("does not unfreeze on native terminal while the picker command is still unsettled", async () => {
  const p = setup({ held: true });
  await screen.findByText(/Preparing profile storage/);
  p.work.status.mockResolvedValue({
    ...idle,
    recent: [{ ...job, state: "cancelled" }],
  });
  await refresh();
  expect(p.onClose).not.toHaveBeenCalled();
  expect(
    screen
      .getByRole("button", { name: "Keep current profile" })
      .hasAttribute("disabled"),
  ).toBe(true);
  await act(async () => p.choice.resolve(null));
  await waitFor(() => expect(p.onClose).toHaveBeenCalledTimes(1));
});
it("reauthorizes the same candidate after save and clears prior approval", async () => {
  const p = setup({ dirty: true });
  await approve();
  fireEvent.click(
    screen.getByRole("button", { name: "Retry save and review profile" }),
  );
  await waitFor(() =>
    expect(p.commands.refresh).toHaveBeenCalledWith("candidate-1", {
      pending: "current UI",
    }),
  );
  expect(p.commands.prepare).toHaveBeenCalledTimes(1);
  expect(p.commands.confirm).not.toHaveBeenCalled();
  expect(
    (
      screen.getByRole("checkbox", {
        name: "I reviewed this profile and its location.",
      }) as HTMLInputElement
    ).checked,
  ).toBe(false);
  await approve();
  fireEvent.click(screen.getByRole("button", { name: "Switch profile" }));
  await waitFor(() =>
    expect(p.commands.confirm).toHaveBeenCalledWith("candidate-2", "saved"),
  );
});
it("exports native current state using the exact token without an unhydrated placeholder", async () => {
  const p = setup({ dirty: true, unhydrated: true });
  await approve();
  fireEvent.click(
    screen.getByRole("button", { name: "Save recovery copy and switch" }),
  );
  await waitFor(() =>
    expect(p.commands.exportCopy).toHaveBeenCalledWith("candidate-1"),
  );
  expect(p.commands.prepare).toHaveBeenCalledWith("open", undefined);
  expect(p.retry).not.toHaveBeenCalled();
});
it("keeps token and disposition fixed after uncertain confirmation", async () => {
  const p = setup({ dirty: true });
  p.commands.confirm.mockRejectedValueOnce(
    new Error("PROFILE_SELECTION_RECOVERY_REQUIRED: lost response"),
  );
  await approve();
  fireEvent.click(
    screen.getByRole("checkbox", {
      name: /Discard only the current unsaved edits/,
    }),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Switch and discard pending edits" }),
  );
  await screen.findByText(/PROFILE_SELECTION_RECOVERY_REQUIRED/);
  expect(
    screen
      .getByRole("button", { name: "Keep current profile" })
      .hasAttribute("disabled"),
  ).toBe(true);
  expect(
    screen
      .getByRole("button", { name: "Save recovery copy and switch" })
      .hasAttribute("disabled"),
  ).toBe(true);
  const retry = screen.getByRole("button", {
    name: "Retry exact profile confirmation",
  });
  await waitFor(() => expect(retry.hasAttribute("disabled")).toBe(false));
  fireEvent.click(retry);
  await waitFor(() => expect(p.commands.confirm).toHaveBeenCalledTimes(2));
  expect(p.commands.confirm.mock.calls).toEqual([
    ["candidate-1", "discard"],
    ["candidate-1", "discard"],
  ]);
});
it("does not offer a native-only copy as protection for unadmitted renderer changes", async () => {
  const p = setup({ dirty: true, unadmitted: true });
  await approve();
  expect(
    screen
      .getByRole("button", { name: "Save recovery copy and switch" })
      .hasAttribute("disabled"),
  ).toBe(true);
  expect(screen.getByText(/newer changes than native storage/)).toBeTruthy();
  expect(p.commands.exportCopy).not.toHaveBeenCalled();
});
