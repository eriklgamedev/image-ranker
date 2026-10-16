// @vitest-environment jsdom
import { StrictMode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { RecoveryReplacement } from "../../src/components/RecoveryReplacement";
import type {
  BootStatus,
  RecoveryReplacementPreview,
  RecoveryReplacementSubject,
} from "../../src/native";
afterEach(cleanup);
const subject: RecoveryReplacementSubject = {
  kind: "fresh",
  token: "inspected",
  acceptPending: true,
};
const pending: RecoveryReplacementPreview = {
  token: "replacement-A",
  subjectToken: "inspected",
  subjectKind: "fresh",
  currentSaveState: "pending",
  hasCurrentSession: true,
  requiresDisposition: true,
  rendererChangesUnadmitted: false,
  pendingLayers: [
    {
      kind: "pending-event",
      id: "event-A",
      summary: "Last vote has uncertain acknowledgment",
    },
    {
      kind: "pending-proposal",
      id: "proposal-B",
      summary: "Later picks remain in memory",
    },
  ],
  warnings: [],
};
const boot = {
  mode: "ready",
  state: "ready",
  issues: [],
  canRestoreBackup: true,
  pendingOperations: 0,
  profileGeneration: "profile-A",
  contextId: "new-context",
  canChooseRecoveryStorage: true,
  profile: {
    id: "profile-A",
    displayPath: "C:/Fixture/Profile",
    isDefault: true,
    writable: true,
  },
} as BootStatus;
const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => (resolve = done));
  return { promise, resolve };
};
function setup(
  overrides: Partial<Parameters<typeof RecoveryReplacement>[0]> = {},
  strict = false,
) {
  const props = {
    subject,
    generation: 1,
    canRetrySave: true,
    commands: {
      prepare: vi.fn().mockResolvedValue(pending),
      cancel: vi.fn().mockResolvedValue(undefined),
      exportCopy: vi.fn().mockResolvedValue("current.json"),
      confirm: vi.fn().mockResolvedValue(boot),
    },
    flush: vi.fn().mockResolvedValue(undefined),
    retry: vi.fn().mockResolvedValue(undefined),
    snapshot: vi.fn(() => ({ name: "current-admitted" })),
    onClose: vi.fn(),
    onRestored: vi.fn(),
    ...overrides,
  };
  const view = render(
    strict ? (
      <StrictMode>
        <RecoveryReplacement {...props} />
      </StrictMode>
    ) : (
      <RecoveryReplacement {...props} />
    ),
  );
  return { props, ...view };
}
const prepared = () =>
  screen.findByRole("button", {
    name: "Save current recovery copy and restore",
  });
it("drains once under StrictMode and binds native preparation to the actual current session", async () => {
  const queued = deferred<void>();
  const { props } = setup({ flush: () => queued.promise }, true);
  expect(props.commands.prepare).not.toHaveBeenCalled();
  await act(async () => queued.resolve());
  await prepared();
  expect(props.commands.prepare).toHaveBeenCalledExactlyOnceWith(subject, {
    name: "current-admitted",
  });
  expect(
    screen.getByText(/Last vote has uncertain acknowledgment/),
  ).toBeTruthy();
  expect(screen.getByText(/Later picks remain in memory/)).toBeTruthy();
  expect(props.commands.confirm).not.toHaveBeenCalled();
});
it("continues a native-confirmed clean session only after the inspected restore was requested", async () => {
  const commands = {
    prepare: vi.fn().mockResolvedValue({
      ...pending,
      requiresDisposition: false,
      currentSaveState: "saved",
      pendingLayers: [],
    }),
    cancel: vi.fn().mockResolvedValue(undefined),
    exportCopy: vi.fn(),
    confirm: vi.fn().mockResolvedValue(boot).mockResolvedValue(boot),
  };
  const { props } = setup({ commands });
  await waitFor(() => expect(props.onRestored).toHaveBeenCalledWith(boot));
  expect(commands.confirm).toHaveBeenCalledExactlyOnceWith(
    subject,
    "replacement-A",
    "saved",
  );
});
it("allows unavailable original storage with no current native session without inventing a saved renderer value", async () => {
  const commands = {
    prepare: vi.fn().mockResolvedValue({
      ...pending,
      currentSaveState: "unavailable",
      hasCurrentSession: false,
      requiresDisposition: false,
      pendingLayers: [],
    }),
    cancel: vi.fn().mockResolvedValue(undefined),
    exportCopy: vi.fn(),
    confirm: vi.fn().mockResolvedValue(boot).mockResolvedValue(boot),
  };
  const { props } = setup({
    commands,
    snapshot: () => undefined,
    canRetrySave: false,
    flush: vi.fn().mockRejectedValue(new Error("original store unavailable")),
  });
  await waitFor(() => expect(props.onRestored).toHaveBeenCalledWith(boot));
  expect(commands.prepare).toHaveBeenCalledExactlyOnceWith(subject, undefined);
  expect(commands.exportCopy).not.toHaveBeenCalled();
  expect(props.retry).not.toHaveBeenCalled();
});
it("keeps both current pending layers separate from imported consent and requires explicit discard", async () => {
  const { props } = setup({
    flush: vi.fn().mockRejectedValue(new Error("append failed")),
  });
  await prepared();
  const discard = screen.getByRole("button", {
    name: "Discard current unsaved edits and restore",
  });
  fireEvent.click(discard);
  expect(props.commands.confirm).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("checkbox"));
  fireEvent.click(discard);
  await waitFor(() =>
    expect(props.commands.confirm).toHaveBeenCalledExactlyOnceWith(
      subject,
      "replacement-A",
      "discard",
    ),
  );
  expect(props.commands.exportCopy).not.toHaveBeenCalled();
});
it("native-only export never claims to capture unadmitted renderer changes", async () => {
  const commands = {
    prepare: vi
      .fn()
      .mockResolvedValue({ ...pending, rendererChangesUnadmitted: true }),
    cancel: vi.fn().mockResolvedValue(undefined),
    exportCopy: vi.fn(),
    confirm: vi.fn().mockResolvedValue(boot),
  };
  setup({ commands });
  await screen.findByText(/latest visible edits have not been admitted/);
  expect(
    screen.queryByRole("button", {
      name: "Save current recovery copy and restore",
    }),
  ).toBeNull();
  expect(commands.confirm).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("checkbox"));
  fireEvent.click(
    screen.getByRole("button", {
      name: "Discard current unsaved edits and restore",
    }),
  );
  await waitFor(() =>
    expect(commands.confirm).toHaveBeenCalledWith(
      subject,
      "replacement-A",
      "discard",
    ),
  );
});
it("cancelled export preserves subject and current layers without confirming replacement", async () => {
  const { props } = setup();
  vi.mocked(props.commands.exportCopy).mockResolvedValue(null);
  fireEvent.click(await prepared());
  await screen.findByText(/Recovery copy cancelled/);
  expect(props.commands.exportCopy).toHaveBeenCalledExactlyOnceWith(
    "replacement-A",
  );
  expect(props.commands.confirm).not.toHaveBeenCalled();
  expect(props.onClose).not.toHaveBeenCalled();
  expect(screen.getByText(/Later picks remain in memory/)).toBeTruthy();
});
it("exports only the native replacement token, then confirms its exact proof disposition", async () => {
  const { props } = setup();
  fireEvent.click(await prepared());
  await waitFor(() => expect(props.onRestored).toHaveBeenCalledWith(boot));
  expect(props.commands.exportCopy).toHaveBeenCalledExactlyOnceWith(
    "replacement-A",
  );
  expect(props.commands.confirm).toHaveBeenCalledExactlyOnceWith(
    subject,
    "replacement-A",
    "recovery-copy",
  );
});
it("save retry reparses no bundle and prepares a new token for the same imported choice", async () => {
  const { props } = setup();
  await prepared();
  vi.mocked(props.commands.prepare).mockResolvedValue({
    ...pending,
    token: "replacement-B",
    currentSaveState: "saved",
    requiresDisposition: false,
    pendingLayers: [],
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Retry current save and restore" }),
  );
  await waitFor(() => expect(props.onRestored).toHaveBeenCalledWith(boot));
  expect(props.retry).toHaveBeenCalledTimes(1);
  expect(props.flush).toHaveBeenCalledTimes(2);
  expect(
    vi.mocked(props.commands.prepare).mock.calls.map((call) => call[0]),
  ).toEqual([subject, subject]);
  expect(props.commands.confirm).toHaveBeenCalledWith(
    subject,
    "replacement-B",
    "saved",
  );
});
it("checkpoint-pending blocks saved confirmation until explicit save retry", async () => {
  const commands = {
    prepare: vi.fn().mockResolvedValue({
      ...pending,
      currentSaveState: "checkpoint-pending",
      pendingLayers: [],
    }),
    cancel: vi.fn().mockResolvedValue(undefined),
    exportCopy: vi.fn(),
    confirm: vi.fn().mockResolvedValue(boot),
  };
  setup({ commands });
  await prepared();
  expect(commands.confirm).not.toHaveBeenCalled();
  expect(screen.getByText("Save state: checkpoint-pending.")).toBeTruthy();
});
it("known precommit rejection retains subject and permits reprepare with reset discard approval", async () => {
  const { props } = setup();
  vi.mocked(props.commands.confirm).mockRejectedValue(
    new Error("RECOVERY_SAVE_REQUIRED: pending state remains"),
  );
  await prepared();
  fireEvent.click(screen.getByRole("checkbox"));
  fireEvent.click(
    screen.getByRole("button", {
      name: "Discard current unsaved edits and restore",
    }),
  );
  await screen.findByText(/RECOVERY_SAVE_REQUIRED/);
  expect(props.onClose).not.toHaveBeenCalled();
  vi.mocked(props.commands.prepare).mockResolvedValue({
    ...pending,
    token: "replacement-B",
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Refresh replacement authorization" }),
  );
  await waitFor(() => expect(props.commands.prepare).toHaveBeenCalledTimes(2));
  expect((screen.getByRole("checkbox") as HTMLInputElement).checked).toBe(
    false,
  );
  expect(props.commands.cancel).not.toHaveBeenCalled();
});
it("unknown lost response permits only the exact token and disposition retry", async () => {
  const { props } = setup();
  vi.mocked(props.commands.confirm).mockRejectedValueOnce(
    new Error("IPC response lost"),
  );
  fireEvent.click(await prepared());
  fireEvent.click(
    await screen.findByRole("button", { name: "Check restore outcome" }),
  );
  await waitFor(() => expect(props.onRestored).toHaveBeenCalledWith(boot));
  expect(vi.mocked(props.commands.confirm).mock.calls).toEqual([
    [subject, "replacement-A", "recovery-copy"],
    [subject, "replacement-A", "recovery-copy"],
  ]);
  expect(props.commands.prepare).toHaveBeenCalledTimes(1);
  expect(props.commands.exportCopy).toHaveBeenCalledTimes(1);
});
it("retained resume never supplies a fresh imported-pending override", async () => {
  const retained: RecoveryReplacementSubject = {
    kind: "resume",
    token: "retained-subject",
  };
  const commands = {
    prepare: vi.fn().mockResolvedValue({
      ...pending,
      subjectKind: "resume",
      subjectToken: retained.token,
    }),
    cancel: vi.fn().mockResolvedValue(undefined),
    exportCopy: vi.fn().mockResolvedValue("current.json"),
    confirm: vi.fn().mockResolvedValue(boot),
  };
  const { props } = setup({ subject: retained, commands });
  fireEvent.click(await prepared());
  await waitFor(() => expect(props.onRestored).toHaveBeenCalledWith(boot));
  expect(commands.confirm).toHaveBeenCalledWith(
    retained,
    "replacement-A",
    "recovery-copy",
  );
});
it("failed cancellation keeps replacement frozen and allows cancellation retry", async () => {
  const { props } = setup();
  vi.mocked(props.commands.cancel).mockRejectedValueOnce(
    new Error("RECOVERY_BUSY"),
  );
  await prepared();
  fireEvent.click(screen.getByRole("button", { name: "Keep current session" }));
  await screen.findByText(/RECOVERY_BUSY/);
  expect(props.onClose).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Keep current session" }));
  await waitFor(() => expect(props.onClose).toHaveBeenCalledTimes(1));
  expect(props.commands.cancel).toHaveBeenCalledTimes(2);
});
it("releases a late preparation after unmount", async () => {
  const pendingResult = deferred<RecoveryReplacementPreview>();
  const commands = {
    prepare: vi.fn(() => pendingResult.promise),
    cancel: vi.fn().mockResolvedValue(undefined),
    exportCopy: vi.fn(),
    confirm: vi.fn().mockResolvedValue(boot),
  };
  const { unmount } = setup({ commands });
  await waitFor(() => expect(commands.prepare).toHaveBeenCalledTimes(1));
  unmount();
  await act(async () => pendingResult.resolve(pending));
  expect(commands.cancel).toHaveBeenCalledExactlyOnceWith("replacement-A");
  expect(commands.confirm).not.toHaveBeenCalled();
});
it("committed reload failure cannot reconfirm, export or discard the old session", async () => {
  const { props } = setup({
    onRestored: vi
      .fn()
      .mockRejectedValueOnce(new Error("Navigation failed"))
      .mockResolvedValue(undefined),
  });
  fireEvent.click(await prepared());
  await screen.findByText(/Navigation failed/);
  fireEvent.click(
    screen.getByRole("button", { name: "Reload selected workspace" }),
  );
  await waitFor(() => expect(props.onRestored).toHaveBeenCalledTimes(2));
  expect(props.commands.confirm).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("checkbox")).toBeNull();
});

it("rejects and releases a preparation for another subject without enabling restore", async () => {
  const { props } = setup();
  vi.mocked(props.commands.prepare).mockResolvedValue({
    ...pending,
    subjectToken: "other-subject",
  });
  await screen.findByText(/STALE_RECOVERY_REPLACEMENT/);
  expect(props.commands.cancel).toHaveBeenCalledExactlyOnceWith(
    "replacement-A",
  );
  expect(props.commands.confirm).not.toHaveBeenCalled();
  expect(screen.queryByRole("checkbox")).toBeNull();
});
it("ignores a preparation from a retired renderer generation", async () => {
  const result = deferred<RecoveryReplacementPreview>();
  const commands = {
    prepare: vi.fn(() => result.promise),
    cancel: vi.fn().mockResolvedValue(undefined),
    exportCopy: vi.fn(),
    confirm: vi.fn(),
  };
  const { props, rerender } = setup({ commands });
  await waitFor(() => expect(commands.prepare).toHaveBeenCalledTimes(1));
  rerender(<RecoveryReplacement {...props} generation={2} />);
  await act(async () => result.resolve(pending));
  expect(commands.cancel).toHaveBeenCalledExactlyOnceWith("replacement-A");
  expect(commands.confirm).not.toHaveBeenCalled();
  expect(screen.queryByRole("checkbox")).toBeNull();
});
it("keeps a failed save retry reviewable and never marks it saved", async () => {
  const { props } = setup();
  await prepared();
  vi.mocked(props.retry).mockRejectedValue(new Error("append still failed"));
  fireEvent.click(
    screen.getByRole("button", { name: "Retry current save and restore" }),
  );
  await screen.findByText(/Current save: Error: append still failed/);
  expect(props.commands.prepare).toHaveBeenCalledTimes(2);
  expect(props.commands.confirm).not.toHaveBeenCalled();
  expect(props.onClose).not.toHaveBeenCalled();
});
it("blocks duplicate export clicks while the exact token is awaiting its picker", async () => {
  const { props } = setup();
  const result = deferred<string | null>();
  vi.mocked(props.commands.exportCopy).mockReturnValue(result.promise);
  const button = await prepared();
  fireEvent.click(button);
  fireEvent.click(button);
  expect(props.commands.exportCopy).toHaveBeenCalledExactlyOnceWith(
    "replacement-A",
  );
  expect(
    screen
      .getByRole("button", { name: "Keep current session" })
      .hasAttribute("disabled"),
  ).toBe(true);
  await act(async () => result.resolve(null));
  expect(props.commands.confirm).not.toHaveBeenCalled();
});
it("does not cancel uncertain authorization on unmount", async () => {
  const { props, unmount } = setup();
  vi.mocked(props.commands.confirm).mockRejectedValueOnce(
    new Error("IPC response lost"),
  );
  fireEvent.click(await prepared());
  await screen.findByRole("button", { name: "Check restore outcome" });
  unmount();
  await act(async () => {});
  expect(props.commands.cancel).not.toHaveBeenCalled();
});
