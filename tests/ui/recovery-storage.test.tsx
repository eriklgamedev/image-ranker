// @vitest-environment jsdom
import { StrictMode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import {
  RecoveryStoragePanel,
  type RecoveryStorageActions,
} from "../../src/components/RecoveryStoragePanel";
import { StartupRecovery } from "../../src/components/StartupRecovery";
import type {
  BootStatus,
  RecoveryResumePreview,
  RecoveryStorageStatus,
} from "../../src/native";
afterEach(cleanup);
it("releases the parent recovery guard when successful loading replaces the recovery screen", async () => {
  let finish!: () => void;
  const onRetry = vi.fn(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
  );
  const onBusyChange = vi.fn();
  const view = render(
    <StartupRecovery
      status={null}
      failure="Storage temporarily unavailable"
      onRetry={onRetry}
      onRestored={() => {}}
      onBusyChange={onBusyChange}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Retry loading" }));
  expect(onBusyChange).toHaveBeenLastCalledWith(true);
  view.unmount();
  expect(onBusyChange).toHaveBeenLastCalledWith(false);
  await act(async () => finish());
});
const status: RecoveryStorageStatus = {
  selectedRequestId: null,
  selectedBranchId: null,
  activationState: "original",
  usedBranches: 1,
  usedBytes: 4096,
  limits: { branches: 8, bytes: 2147483648, requests: 1024 },
  warnings: [],
  requests: [
    {
      requestId: "retained-request",
      branchId: "retained-branch",
      state: "staged",
      pendingActions: 1,
      acceptedPending: false,
      bytes: 4096,
      canResume: true,
      canCleanup: true,
      reason: null,
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
    id: "profile",
    displayPath: "profile",
    isDefault: false,
    writable: true,
  },
  contextId: "new-context",
  profileGeneration: "new-generation",
  canChooseRecoveryStorage: true,
};
const resume: RecoveryResumePreview = {
  token: "resume-native-token",
  requestId: "retained-request",
  branchId: "retained-branch",
  acceptedPending: false,
  action: "resume-bootstrap",
  warnings: ["Same retained branch."],
  inspection: {
    token: "durable-request",
    createdAt: "2026-09-29",
    summary: { images: 2, runs: 1, decisions: 1, reviewEdits: 0 },
    restorable: true,
    missingJournals: [],
    missingBackups: [],
    savedRevision: 1,
    pendingActions: 1,
    pendingSummary: ["Captured vote is retained separately."],
    warnings: ["Reconnect sources after restore."],
  },
};
function setup() {
  const actions = {
    loadStatus: vi.fn().mockResolvedValue(status),
    prepareResume: vi.fn().mockResolvedValue(resume),
    confirmResume: vi.fn().mockResolvedValue(boot),
    cancelResume: vi.fn().mockResolvedValue(undefined),
    previewCleanup: vi
      .fn()
      .mockResolvedValue({
        token: "cleanup-native-token",
        requestId: "retained-request",
        branchId: "retained-branch",
        totalBytes: 512,
        files: [{ relativePath: "sessions/owned.json", sizeBytes: 512 }],
        preserved: [
          { relativePath: "foreign.txt", reason: "No ownership evidence" },
        ],
        warnings: [],
      }),
    confirmCleanup: vi
      .fn()
      .mockResolvedValue({
        ...status,
        usedBranches: 0,
        requests: [
          {
            ...status.requests[0],
            state: "retired",
            canResume: false,
            canCleanup: false,
          },
        ],
      }),
    cancelCleanup: vi.fn().mockResolvedValue(undefined),
  } satisfies RecoveryStorageActions;
  return { actions, onRestored: vi.fn(), onBusyChange: vi.fn() };
}
async function loaded() {
  await screen.findByText(/1 of 8 staged branches/);
}
it("retries a status failure and exposes blocked reasons without enabling unsafe actions", async () => {
  const props = setup();
  props.actions.loadStatus.mockRejectedValueOnce(new Error("RECOVERY_BUSY"));
  props.actions.loadStatus.mockResolvedValue({
    ...status,
    requests: [
      {
        ...status.requests[0],
        state: "blocked",
        canResume: false,
        canCleanup: false,
        reason: "Ownership changed",
      },
    ],
  });
  render(<RecoveryStoragePanel {...props} />);
  expect((await screen.findByRole("alert")).textContent).toContain(
    "RECOVERY_BUSY",
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Refresh recovery status" }),
  );
  await loaded();
  expect(screen.getByText("Ownership changed")).toBeTruthy();
  expect(
    (
      screen.getByRole("button", {
        name: "Preview resume",
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  expect(
    (
      screen.getByRole("button", {
        name: "Preview branch cleanup",
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
});
it("uses the exact native preview token and only retries UI reload after native commit", async () => {
  const props = setup();
  props.onRestored
    .mockRejectedValueOnce(new Error("Reload failed"))
    .mockResolvedValue(undefined);
  render(<RecoveryStoragePanel {...props} />);
  await loaded();
  fireEvent.click(screen.getByRole("button", { name: "Preview resume" }));
  await screen.findByText(
    /original pending-action choice is preserved: restore saved state only/,
  );
  expect(props.actions.prepareResume).toHaveBeenCalledWith("retained-request");
  expect(props.actions.confirmResume).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Confirm resume" }));
  expect((await screen.findByRole("alert")).textContent).toContain(
    "Reload failed",
  );
  expect(props.actions.confirmResume).toHaveBeenCalledWith(
    "resume-native-token",
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Reload selected workspace" }),
  );
  await waitFor(() => expect(props.onRestored).toHaveBeenCalledTimes(2));
  expect(props.actions.confirmResume).toHaveBeenCalledTimes(1);
  expect(props.actions.cancelResume).not.toHaveBeenCalled();
});
it("shows owned and preserved cleanup files and records confirmation once", async () => {
  const props = setup();
  render(<RecoveryStoragePanel {...props} />);
  await loaded();
  fireEvent.click(
    screen.getByRole("button", { name: "Preview branch cleanup" }),
  );
  const preview = await screen.findByLabelText("Branch cleanup preview");
  expect(within(preview).getByText("foreign.txt")).toBeTruthy();
  expect(within(preview).getByText("sessions/owned.json")).toBeTruthy();
  expect(props.actions.confirmCleanup).not.toHaveBeenCalled();
  const confirm = screen.getByRole("button", { name: "Confirm cleanup" });
  fireEvent.click(confirm);
  fireEvent.click(confirm);
  await screen.findByText(/Cleanup recorded/);
  expect(props.actions.confirmCleanup).toHaveBeenCalledExactlyOnceWith(
    "cleanup-native-token",
  );
  expect(screen.getByText("retired")).toBeTruthy();
});
it("cancels a cleanup preview without deleting and preserves the preview on cancel failure", async () => {
  const props = setup();
  props.actions.cancelCleanup.mockRejectedValueOnce(new Error("Native busy"));
  render(<RecoveryStoragePanel {...props} />);
  await loaded();
  fireEvent.click(
    screen.getByRole("button", { name: "Preview branch cleanup" }),
  );
  fireEvent.click(
    await screen.findByRole("button", { name: "Cancel preview" }),
  );
  expect((await screen.findByRole("alert")).textContent).toContain(
    "Native busy",
  );
  expect(screen.getByText("Confirm staged-branch cleanup")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Cancel preview" }));
  await waitFor(() =>
    expect(screen.queryByText("Confirm staged-branch cleanup")).toBeNull(),
  );
  expect(props.actions.confirmCleanup).not.toHaveBeenCalled();
  expect(props.actions.cancelCleanup).toHaveBeenLastCalledWith(
    "cleanup-native-token",
  );
});
it("releases a late prepared preview after unmount without confirmation", async () => {
  const props = setup();
  let finish!: (value: RecoveryResumePreview) => void;
  props.actions.prepareResume.mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const view = render(<RecoveryStoragePanel {...props} />);
  await loaded();
  fireEvent.click(screen.getByRole("button", { name: "Preview resume" }));
  view.unmount();
  await act(async () => finish(resume));
  expect(props.actions.cancelResume).toHaveBeenCalledWith(
    "resume-native-token",
  );
  expect(props.actions.confirmResume).not.toHaveBeenCalled();
});
it("finishes initial discovery under StrictMode and releases its busy guard", async () => {
  const props = setup();
  render(
    <StrictMode>
      <RecoveryStoragePanel {...props} />
    </StrictMode>,
  );
  await loaded();
  await waitFor(() =>
    expect(
      (
        screen.getByRole("button", {
          name: "Preview resume",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(false),
  );
  expect(props.onBusyChange).toHaveBeenLastCalledWith(false);
});
