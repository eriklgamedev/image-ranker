// @vitest-environment jsdom
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import App from "../../src/App";
import {
  StartupRecovery,
  type RecoveryInspectionView,
} from "../../src/components/StartupRecovery";
import { createRun, getPair } from "../../src/domain/ranking";
import { demoReport } from "../../src/components/demo";
const mocks = vi.hoisted(() => ({
  appInfo: vi.fn(),
  licenseNotices: vi.fn(),
  storageLocations: vi.fn(),
  onActivationRestriction: vi.fn(),
  onOperationRestriction: vi.fn(),
  retryOperationPublication: vi.fn(),
  prepareOperationRestart: vi.fn(),
  exportOperationRestart: vi.fn(),
  confirmOperationRestart: vi.fn(),
  cancelOperationRestart: vi.fn(),
  recoveryStorageStatus: vi.fn(),
  protectionMaintenanceStatus: vi.fn(),
  previewProtectionMaintenance: vi.fn(),
  confirmProtectionMaintenance: vi.fn(),
  resumeProtectionMaintenance: vi.fn(),
  recoveryWorkStatus: vi.fn(),
  cancelRecoveryWork: vi.fn(),
  loadSession: vi.fn(),
  loadSessionState: vi.fn(),
  rendererContext: vi.fn(),
  chooseRecoveryStorage: vi.fn(),
  openRecoveryProfile: vi.fn(),
  refreshProfileSwitch: vi.fn(),
  exportProfileReplacementCopy: vi.fn(),
  confirmProfileSwitch: vi.fn(),
  cancelProfileSwitch: vi.fn(),
  reloadSelectedProfile: vi.fn(),
  sessionSaveStatus: vi.fn(),
  saveSession: vi.fn(),
  exportSessionCopy: vi.fn(),
  recoveryNotes: vi.fn(),
  listOperations: vi.fn(),
  requestPreview: vi.fn(),
  cancelImageWork: vi.fn().mockResolvedValue(1),
  releasePreview: vi.fn(),
  getBootStatus: vi.fn(),
  retryBoot: vi.fn(),
  inspectRecoveryCopy: vi.fn(),
  restoreRecoveryCopy: vi.fn(),
  prepareRecoveryReplacement: vi.fn(),
  cancelRecoveryReplacement: vi.fn(),
  exportRecoveryReplacementCopy: vi.fn(),
  prepareRecoveryResume: vi.fn(),
  confirmRecoveryResume: vi.fn(),
  cancelRecoveryResume: vi.fn(),
  reconnectSource: vi.fn(),
  subscribeCloseRequested: vi.fn(),
  confirmClose: vi.fn(),
  cancelClose: vi.fn(),
  cancelScan: vi.fn(),
  cancelOperation: vi.fn(),
  recoverOperation: vi.fn(),
  subscribeOperationProgress: vi.fn(),
  cacheInfo: vi.fn(),
  subscribeScanProgress: vi.fn(),
  scanSources: vi.fn(),
}));
vi.mock("../../src/native", () => ({
  desktop: true,
  previewUrl: (item: { previewPath: string }) => item.previewPath,
  api: mocks,
}));
vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string) => path,
}));
vi.mock("../../src/components/VirtualGrid", () => ({
  VirtualGrid: ({
    ids,
    render: renderItem,
  }: {
    ids: string[];
    render: (id: string, index: number) => ReactNode;
  }) => (
    <div>
      {ids.map((id, index) => (
        <div key={id}>{renderItem(id, index)}</div>
      ))}
    </div>
  ),
}));
const boot = {
  mode: "ready" as const,
  issues: [],
  canRestoreBackup: true,
  contextId: "renderer-A",
  profileGeneration: "profile-generation-A",
  canChooseRecoveryStorage: true,
  profile: {
    id: "profile-A",
    displayPath: "C:/Original/Profile",
    isDefault: true,
    writable: true,
  },
};
const envelope = (
  session: unknown,
  saveStatus: unknown = { state: "saved", durableRevision: 3 },
) => ({
  session,
  saveStatus,
  contextId: boot.contextId,
  profileGeneration: boot.profileGeneration,
  sessionGeneration: "session-generation-A",
});
function savedSession() {
  const report = demoReport();
  report.images = report.images.slice(0, 3);
  report.sources[0].count = 3;
  return {
    schemaVersion: 1,
    name: "Recovery fixture",
    report,
    selected: report.images.map((image) => image.id),
    incomingSelected: true,
    run: createRun(
      report.images.map((image) => image.id),
      "full-pairwise",
      8,
      false,
    ),
    review: null,
    archivedRuns: [],
    screen: "arena",
    settings: {
      recursive: false,
      randomize: false,
      mode: "full-pairwise",
      cardSize: 172,
      showNames: true,
    },
  };
}
const inspection: RecoveryInspectionView = {
  token: "native-validated-token",
  createdAt: "2026-09-29 12:00",
  summary: { images: 3, runs: 2, decisions: 9, reviewEdits: 2 },
  warnings: ["Choose the source folders again after restoring."],
  restorable: true,
  missingJournals: ["operation-old"],
  missingBackups: ["replacement-backup"],
  savedRevision: 9,
  pendingActions: 1,
  pendingSummary: [
    "Prefer left in comparison 10; this action was captured without an acknowledgment.",
  ],
};
beforeEach(() => {
  vi.resetAllMocks();
  mocks.appInfo.mockRejectedValue(
    new Error("Bundled metadata unavailable in this fixture"),
  );
  mocks.licenseNotices.mockResolvedValue([]);
  mocks.storageLocations.mockResolvedValue([]);
  mocks.onActivationRestriction.mockReturnValue(() => {});
  mocks.onOperationRestriction.mockReturnValue(() => {});
  mocks.retryOperationPublication.mockResolvedValue(undefined);
  mocks.prepareOperationRestart.mockResolvedValue({
    token: "restart-token-A",
    requestId: "panicked-file-worker-A",
    savedRevision: 3,
    pendingActions: 2,
    rendererProposal: false,
    copyReady: false,
    copyPath: null,
    warnings: ["The file journal must be reconciled after reopening."],
  });
  mocks.exportOperationRestart.mockResolvedValue({
    token: "restart-token-A",
    requestId: "panicked-file-worker-A",
    savedRevision: 3,
    pendingActions: 2,
    rendererProposal: false,
    copyReady: true,
    copyPath: "C:/Recovery/restart-copy.json",
    warnings: ["The file journal must be reconciled after reopening."],
  });
  mocks.confirmOperationRestart.mockResolvedValue(undefined);
  mocks.cancelOperationRestart.mockResolvedValue(undefined);
  mocks.recoveryWorkStatus.mockResolvedValue({
    active: null,
    recent: [],
    limit: 1,
  });
  mocks.recoveryStorageStatus.mockResolvedValue({
    selectedRequestId: null,
    selectedBranchId: null,
    activationState: "original",
    usedBranches: 0,
    usedBytes: 0,
    limits: { branches: 8, bytes: 2147483648, requests: 1024 },
    requests: [],
    warnings: [],
  });
  mocks.loadSessionState.mockResolvedValue(envelope(savedSession()));
  mocks.rendererContext.mockResolvedValue(boot.contextId);
  mocks.cancelProfileSwitch.mockResolvedValue(undefined);
  mocks.exportProfileReplacementCopy.mockResolvedValue("profile-copy.json");
  mocks.chooseRecoveryStorage.mockResolvedValue(null);
  mocks.openRecoveryProfile.mockResolvedValue(null);
  mocks.reloadSelectedProfile.mockReturnValue(undefined);
  mocks.prepareRecoveryReplacement.mockImplementation(async (subject) => ({
    token: "replacement-authorization",
    subjectToken: subject.token,
    subjectKind: subject.kind,
    currentSaveState: "saved",
    hasCurrentSession: false,
    requiresDisposition: false,
    rendererChangesUnadmitted: false,
    pendingLayers: [],
    warnings: [],
  }));
  mocks.cancelRecoveryReplacement.mockResolvedValue(undefined);
  mocks.exportRecoveryReplacementCopy.mockResolvedValue(
    "current-recovery.json",
  );
  mocks.getBootStatus.mockResolvedValue(boot);
  mocks.retryBoot.mockResolvedValue(boot);
  mocks.recoveryNotes.mockResolvedValue([]);
  mocks.listOperations.mockResolvedValue([]);
  mocks.subscribeCloseRequested.mockResolvedValue(() => {});
  mocks.subscribeOperationProgress.mockResolvedValue(() => {});
  mocks.confirmClose.mockResolvedValue(undefined);
  mocks.cancelClose.mockResolvedValue(undefined);
  mocks.releasePreview.mockResolvedValue(undefined);
  mocks.cancelImageWork.mockResolvedValue(1);
  mocks.cacheInfo.mockResolvedValue({
    pinnedFiles: 0,
    pinnedBytes: 0,
    files: 0,
    sizeBytes: 0,
    budgetBytes: 1024 * 1048576,
  });
  mocks.saveSession.mockRejectedValue(new Error("Session storage is full"));
  mocks.requestPreview.mockImplementation(
    async (
      id: string,
      _size: number,
      contentRevision: string,
      requestId: string,
    ) => ({
      path: demoReport().images.find((image) => image.id === id)!.previewPath,
      contentRevision,
      requestId,
      leaseId: `lease-${requestId}`,
      assetToken: id,
      width: 2048,
      height: 1024,
      scale: 1,
      pipelineVersion: "v1",
      status: "ready",
    }),
  );
});
afterEach(cleanup);
async function readyRecoveryButton(name: string) {
  const button = await screen.findByRole("button", { name });
  await act(async () => {});
  await waitFor(() => expect(button.hasAttribute("disabled")).toBe(false));
  return button;
}
describe("off-lock scan staging", () => {
  it("keeps progressive selections local and saves only IDs admitted by the completed native scan", async () => {
    const session = savedSession();
    session.screen = "sources";
    mocks.loadSessionState.mockResolvedValue(envelope(session));
    mocks.saveSession.mockResolvedValue(undefined);
    let progress!: (event: {
      scanned: number;
      eligible: number;
      sourceId: string;
      batch: ReturnType<typeof demoReport>["images"];
      done: boolean;
      cancelled: boolean;
    }) => void;
    let finish!: (value: ReturnType<typeof demoReport>) => void;
    mocks.subscribeScanProgress.mockImplementation(async (handler) => {
      progress = handler;
      return () => {};
    });
    mocks.scanSources.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    render(<App />);
    await screen.findByText("Recovery fixture");
    await waitFor(() =>
      expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
        false,
      ),
    );
    fireEvent.click(screen.getByRole("button", { name: "Add folder" }));
    await waitFor(() => expect(mocks.scanSources).toHaveBeenCalledTimes(1));
    const arrivals = demoReport().images.slice(3, 6);
    await act(async () => {
      progress({
        scanned: 4,
        eligible: 4,
        sourceId: session.report.sources[0].id,
        batch: [arrivals[0]],
        done: false,
        cancelled: false,
      });
    });
    const first = await screen.findByRole("checkbox", {
      name: `Select ${arrivals[0].originalName}`,
    });
    expect((first as HTMLInputElement).checked).toBe(true);
    expect(
      screen
        .getByRole("button", { name: `Inspect ${arrivals[0].originalName}` })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(
      mocks.requestPreview.mock.calls.some(
        (call) => call[0] === arrivals[0].id,
      ),
    ).toBe(false);
    fireEvent.click(first);
    expect((first as HTMLInputElement).checked).toBe(false);
    expect(mocks.saveSession).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "None" }));
    await waitFor(() => expect(mocks.saveSession).toHaveBeenCalledTimes(1));
    const savedDuringScan = mocks.saveSession.mock.calls[0][0];
    expect(savedDuringScan.selected).toEqual([]);
    expect(savedDuringScan.incomingSelected).toBe(false);
    expect(savedDuringScan.report.images).toHaveLength(3);
    await act(async () => {
      progress({
        scanned: 6,
        eligible: 6,
        sourceId: session.report.sources[0].id,
        batch: arrivals.slice(1),
        done: false,
        cancelled: false,
      });
    });
    const second = await screen.findByRole("checkbox", {
      name: `Select ${arrivals[1].originalName}`,
    });
    const omitted = screen.getByRole("checkbox", {
      name: `Select ${arrivals[2].originalName}`,
    });
    expect((second as HTMLInputElement).checked).toBe(false);
    fireEvent.click(second);
    fireEvent.click(omitted);
    expect(mocks.saveSession).toHaveBeenCalledTimes(1);
    const final = {
      ...session.report,
      images: [...session.report.images, ...arrivals.slice(0, 2)],
      cancelled: true,
    };
    await act(async () => {
      finish(final);
    });
    await waitFor(() => expect(mocks.saveSession).toHaveBeenCalledTimes(2));
    const saved = mocks.saveSession.mock.calls[1][0];
    expect(saved.selected).toEqual([arrivals[1].id]);
    expect(saved.report.images).toHaveLength(5);
    expect(saved.run).toEqual(session.run);
    expect(
      screen.queryByRole("checkbox", {
        name: `Select ${arrivals[2].originalName}`,
      }),
    ).toBeNull();
    expect(
      mocks.requestPreview.mock.calls.some(
        (call) => call[0] === arrivals[1].id,
      ),
    ).toBe(true);
  });
  it("discards uncommitted scan overlay on native failure without writing unknown images", async () => {
    const session = savedSession();
    session.screen = "sources";
    mocks.loadSessionState.mockResolvedValue(envelope(session));
    mocks.saveSession.mockResolvedValue(undefined);
    let progress!: (event: unknown) => void, fail!: (reason: Error) => void;
    mocks.subscribeScanProgress.mockImplementation(async (handler) => {
      progress = handler;
      return () => {};
    });
    mocks.scanSources.mockImplementation(
      () =>
        new Promise((_resolve, reject) => {
          fail = reject;
        }),
    );
    render(<App />);
    await screen.findByText("Recovery fixture");
    await waitFor(() =>
      expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
        false,
      ),
    );
    fireEvent.click(screen.getByRole("button", { name: "Add folder" }));
    await waitFor(() => expect(mocks.scanSources).toHaveBeenCalledTimes(1));
    const incoming = demoReport().images[3];
    await act(async () => {
      progress({
        scanned: 4,
        eligible: 4,
        sourceId: session.report.sources[0].id,
        batch: [incoming],
        done: false,
        cancelled: false,
      });
    });
    await screen.findByRole("checkbox", {
      name: `Select ${incoming.originalName}`,
    });
    await act(async () => {
      fail(new Error("STALE_CATALOG: scan was not committed"));
    });
    await screen.findByText(/STALE_CATALOG/);
    expect(
      screen.queryByRole("checkbox", {
        name: `Select ${incoming.originalName}`,
      }),
    ).toBeNull();
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
});
async function failVote() {
  render(<App />);
  await screen.findByText("Which image do you prefer?");
  await act(async () => {});
  for (const image of screen.getAllByRole("img")) fireEvent.load(image);
  await waitFor(() =>
    expect(
      screen
        .getByRole("button", { name: /Prefer left/ })
        .hasAttribute("disabled"),
    ).toBe(false),
  );
  fireEvent.click(screen.getByRole("button", { name: /Prefer left/ }));
  await screen.findByRole("button", { name: "Save recovery copy" });
}
describe("save-failure recovery copy", () => {
  it("keeps the exact pending snapshot exportable when activation becomes uncertain", async () => {
    const pending = savedSession();
    pending.name = "Pending activation memory";
    mocks.loadSessionState.mockResolvedValue(
      envelope(pending, { state: "pending", durableRevision: 7 }),
    );
    mocks.exportSessionCopy.mockResolvedValue("D:/Recovery/retained.json");
    render(<App />);
    await screen.findByRole("button", { name: "Retry save" });
    const listener = mocks.onActivationRestriction.mock.calls[0][0];
    await act(async () =>
      listener({
        ...boot,
        mode: "recovery",
        state: "read-only-recovery",
        issues: [
          {
            code: "activation-unresolved",
            message: "The branch commit needs inspection.",
          },
        ],
        trustedOperationsAvailable: false,
      }),
    );
    await screen.findByText("Your workspace needs recovery");
    fireEvent.click(screen.getByRole("button", { name: "Save recovery copy" }));
    await waitFor(() =>
      expect(mocks.exportSessionCopy).toHaveBeenCalledExactlyOnceWith(pending),
    );
    expect(mocks.reloadSelectedProfile).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
    expect(mocks.loadSessionState).toHaveBeenCalledTimes(1);
  });
  it("exports the full pending UI snapshot without claiming the failed app save succeeded", async () => {
    mocks.exportSessionCopy.mockResolvedValue("D:/Recovery/session.irbackup");
    await failVote();
    fireEvent.click(screen.getByRole("button", { name: "Save recovery copy" }));
    await screen.findByText("Session recovery copy saved.");
    const exported = mocks.exportSessionCopy.mock.calls[0][0];
    expect(exported.run.history.length).toBe(1);
    expect(exported.archivedRuns).toEqual([]);
    expect(exported.report.images).toHaveLength(3);
    expect(getPair(exported.run)?.id).not.toBe(getPair(savedSession().run)?.id);
    expect(screen.getByRole("button", { name: "Retry save" })).toBeTruthy();
    expect(
      screen
        .getByRole("button", { name: /Prefer left/ })
        .hasAttribute("disabled"),
    ).toBe(true);
  });
  it("treats save-dialog cancellation as cancellation while preserving pending work", async () => {
    mocks.exportSessionCopy.mockResolvedValue(null);
    await failVote();
    fireEvent.click(screen.getByRole("button", { name: "Save recovery copy" }));
    await screen.findByText(
      "Recovery copy cancelled. Your work remains in memory.",
    );
    expect(screen.queryByText("Session recovery copy saved.")).toBeNull();
    expect(mocks.exportSessionCopy.mock.calls[0][0].run.history.length).toBe(1);
  });
  it("reports copy failure and supports retry without appending the decision again", async () => {
    mocks.exportSessionCopy
      .mockRejectedValueOnce(new Error("Destination has no space"))
      .mockResolvedValueOnce("D:/Recovery/safe.irbackup");
    await failVote();
    fireEvent.click(screen.getByRole("button", { name: "Save recovery copy" }));
    await screen.findByText("Error: Destination has no space");
    fireEvent.click(screen.getByRole("button", { name: "Save recovery copy" }));
    await screen.findByText("Session recovery copy saved.");
    expect(mocks.exportSessionCopy.mock.calls[0][0]).toEqual(
      mocks.exportSessionCopy.mock.calls[1][0],
    );
    expect(mocks.saveSession).toHaveBeenCalledTimes(1);
  });
});
describe("native save-state hydration", () => {
  it("marks a healthy native session saved only after its status is confirmed", async () => {
    render(<App />);
    await screen.findByText("Which image do you prefer?");
    expect(document.querySelector(".save-status")?.textContent).toContain(
      "Saved locally",
    );
    expect(mocks.loadSessionState).toHaveBeenCalledExactlyOnceWith();
    expect(mocks.loadSession).not.toHaveBeenCalled();
    expect(mocks.sessionSaveStatus).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
  it("retains pending settings after reload without claiming they were durably saved", async () => {
    const pending = savedSession();
    pending.name = "Pending collection name";
    pending.settings.showNames = false;
    mocks.loadSessionState.mockResolvedValue({
      ...envelope(pending),
      saveStatus: {
        state: "pending",
        durableRevision: 8,
        pendingEventId: "event-not-acked",
        message: "Session append is still pending.",
      },
    });
    // These independent diagnostic probes deliberately disagree. Hydration
    // must use the paired native envelope and never combine separate reads.
    mocks.loadSession.mockResolvedValue(savedSession());
    mocks.sessionSaveStatus.mockResolvedValue({
      state: "saved",
      durableRevision: 9,
    });
    render(<App />);
    await screen.findByText("Which image do you prefer?");
    expect(document.querySelector(".save-status")?.textContent).toContain(
      "Save failed",
    );
    expect(document.querySelector(".save-status")?.textContent).not.toContain(
      "Saved locally",
    );
    expect(screen.getByText(/Session append is still pending/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(
      (screen.getByLabelText("Collection name") as HTMLInputElement).value,
    ).toBe(pending.name);
    expect(
      (
        screen.getByLabelText(
          "Show filenames below comparison images",
        ) as HTMLInputElement
      ).checked,
    ).toBe(false);
    expect(mocks.loadSession).not.toHaveBeenCalled();
    expect(mocks.sessionSaveStatus).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
  it("does not substitute browser data or a fresh saved session for an incomplete native envelope", async () => {
    mocks.loadSessionState.mockResolvedValue(null);
    render(<App />);
    await screen.findByText("Your workspace needs recovery");
    expect(
      screen.getByText(/native session hydration response is incomplete/),
    ).toBeTruthy();
    expect(screen.queryByText("Saved locally")).toBeNull();
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
  it("keeps checkpoint-pending work failed and prevents automatic review writes during hydration", async () => {
    const pending = savedSession();
    pending.run = createRun(
      [pending.report.images[0].id],
      "full-pairwise",
      8,
      false,
    );
    mocks.loadSessionState.mockResolvedValue({
      ...envelope(pending),
      saveStatus: { state: "checkpoint-pending", durableRevision: 100 },
    });
    render(<App />);
    await screen.findByRole("button", { name: "Retry save" });
    expect(document.querySelector(".save-status")?.textContent).toContain(
      "Save failed",
    );
    expect(screen.getByText(/The session checkpoint is pending/)).toBeTruthy();
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
  it("keeps an unavailable atomic read restricted and blocks close without inventing a saved session", async () => {
    const pending = savedSession();
    pending.name = "Still available in memory";
    mocks.loadSessionState.mockRejectedValueOnce(
      new Error("SESSION_SAVE_STATUS_UNAVAILABLE"),
    );
    render(<App />);
    await screen.findByText("Your workspace needs recovery");
    expect(screen.getByText(/SESSION_SAVE_STATUS_UNAVAILABLE/)).toBeTruthy();
    expect(screen.queryByText("Saved locally")).toBeNull();
    expect(mocks.saveSession).not.toHaveBeenCalled();
    await act(async () =>
      mocks.subscribeCloseRequested.mock.calls[0][0]({
        token: "unavailable-close",
      }),
    );
    await screen.findByRole("button", { name: "Retry save and close" });
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Stay open" }));
    await waitFor(() =>
      expect(mocks.cancelClose).toHaveBeenCalledWith("unavailable-close"),
    );
    mocks.loadSessionState.mockResolvedValue({
      ...envelope(pending),
      saveStatus: { state: "pending", durableRevision: 8 },
    });
    fireEvent.click(await readyRecoveryButton("Retry loading"));
    await screen.findByText("Which image do you prefer?");
    expect(document.querySelector(".save-status")?.textContent).toContain(
      "Save failed",
    );
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
  it("blocks close after reload until the exact hydrated pending state is retried", async () => {
    const pending = savedSession();
    pending.settings.showNames = false;
    mocks.loadSessionState.mockResolvedValue({
      ...envelope(pending),
      saveStatus: { state: "pending", durableRevision: 8 },
    });
    mocks.saveSession.mockResolvedValue(undefined);
    const unhandled = vi.fn();
    window.addEventListener("unhandledrejection", unhandled);
    try {
      render(<App />);
      await screen.findByRole("button", { name: "Retry save" });
      await act(async () =>
        mocks.subscribeCloseRequested.mock.calls[0][0]({
          token: "reloaded-close",
        }),
      );
      await screen.findByRole("button", { name: "Retry save and close" });
      expect(mocks.confirmClose).not.toHaveBeenCalled();
      fireEvent.click(
        screen.getByRole("button", { name: "Retry save and close" }),
      );
      await waitFor(() =>
        expect(mocks.confirmClose).toHaveBeenCalledWith(
          "reloaded-close",
          "saved",
        ),
      );
      expect(mocks.saveSession).toHaveBeenCalledExactlyOnceWith(pending);
      expect(unhandled).not.toHaveBeenCalled();
    } finally {
      window.removeEventListener("unhandledrejection", unhandled);
    }
  });
  it("makes close wait for the atomic hydration envelope and rejects its pending result", async () => {
    let resolve!: (status: unknown) => void;
    mocks.loadSessionState.mockImplementation(
      () =>
        new Promise((accept) => {
          resolve = accept;
        }),
    );
    render(<App />);
    await waitFor(() =>
      expect(mocks.loadSessionState).toHaveBeenCalledTimes(1),
    );
    await act(async () =>
      mocks.subscribeCloseRequested.mock.calls[0][0]({
        token: "hydrating-close",
      }),
    );
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    await act(async () =>
      resolve({
        ...envelope(savedSession()),
        saveStatus: { state: "checkpoint-pending", durableRevision: 100 },
      }),
    );
    await screen.findByRole("button", { name: "Retry save and close" });
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
});
describe("profile switching integration", () => {
  const target = {
    id: "profile-B",
    displayPath: "D:/Recovered/Profile",
    isDefault: false,
    writable: true,
  };
  const preview = {
    token: "granted-profile-token",
    mode: "create",
    profile: target,
    hasPendingSave: false,
    pendingOperations: 0,
    requiresDisposition: false,
    warnings: [],
  };
  const targetBoot = {
    ...boot,
    profile: target,
    profileGeneration: "profile-generation-B",
    contextId: "renderer-B",
  };
  async function openSettings() {
    fireEvent.click(await screen.findByRole("button", { name: "Settings" }));
  }
  async function choose() {
    fireEvent.click(
      screen.getByRole("button", { name: "Choose recovery storage" }),
    );
    await screen.findByRole("checkbox", {
      name: "I reviewed this profile and its location.",
    });
  }
  async function approveSwitch() {
    const approval = screen.getByRole("checkbox", {
      name: "I reviewed this profile and its location.",
    });
    await waitFor(() => expect(approval.hasAttribute("disabled")).toBe(false));
    fireEvent.click(approval);
    const confirm = screen.getByRole("button", { name: "Switch profile" });
    await waitFor(() => expect(confirm.hasAttribute("disabled")).toBe(false));
    fireEvent.click(confirm);
  }
  it("offers both picker-granted routes when the default profile cannot be loaded", async () => {
    mocks.getBootStatus.mockResolvedValue({
      ...boot,
      mode: "recovery",
      state: "storage-unavailable",
      profile: { ...boot.profile, writable: false },
    });
    render(<App />);
    await screen.findByText("Your workspace needs recovery");
    await readyRecoveryButton("Choose recovery storage");
    fireEvent.click(await readyRecoveryButton("Open existing profile"));
    await waitFor(() =>
      expect(mocks.openRecoveryProfile).toHaveBeenCalledTimes(1),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
    expect(screen.getByText(boot.profile.displayPath)).toBeTruthy();
  });
  it("retains healthy state and profile identity when preparation fails or is cancelled", async () => {
    mocks.chooseRecoveryStorage.mockRejectedValueOnce(
      new Error("PROFILE_LOCKED"),
    );
    render(<App />);
    await openSettings();
    const name = screen.getByLabelText("Collection name") as HTMLInputElement;
    fireEvent.click(
      screen.getByRole("button", { name: "Choose recovery storage" }),
    );
    await screen.findByText("Error: PROFILE_LOCKED");
    expect(name.value).toBe(savedSession().name);
    await waitFor(() =>
      expect(
        screen
          .getByRole("button", { name: "Keep current profile" })
          .hasAttribute("disabled"),
      ).toBe(false),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Keep current profile" }),
    );
    await waitFor(() =>
      expect(screen.queryByText("Error: PROFILE_LOCKED")).toBeNull(),
    );
    await waitFor(() => expect(screen.getAllByRole("dialog")).toHaveLength(1));
    fireEvent.click(
      screen.getByRole("button", { name: "Open existing profile" }),
    );
    await waitFor(() => expect(mocks.openRecoveryProfile).toHaveBeenCalled());
    await waitFor(() => expect(screen.getAllByRole("dialog")).toHaveLength(1));
    expect(mocks.confirmProfileSwitch).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
    expect(name.value).toBe(savedSession().name);
  });
  it("freezes synchronously and drains a queued setting save before opening the native picker", async () => {
    const pending = savedSession();
    pending.screen = "sources";
    mocks.loadSessionState.mockResolvedValue(envelope(pending));
    let acknowledge!: () => void;
    mocks.saveSession.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          acknowledge = resolve;
        }),
    );
    mocks.chooseRecoveryStorage.mockResolvedValue(preview);
    render(<App />);
    await openSettings();
    const name = screen.getByLabelText("Collection name") as HTMLInputElement;
    fireEvent.change(name, { target: { value: "Queued name" } });
    await waitFor(() => expect(mocks.saveSession).toHaveBeenCalledTimes(1));
    fireEvent.click(
      screen.getByRole("button", { name: "Choose recovery storage" }),
    );
    fireEvent.change(name, { target: { value: "Blocked while switching" } });
    expect(name.value).toBe("Queued name");
    expect(mocks.chooseRecoveryStorage).not.toHaveBeenCalled();
    await act(async () => acknowledge());
    await screen.findByRole("checkbox", {
      name: "I reviewed this profile and its location.",
    });
    expect(mocks.chooseRecoveryStorage).toHaveBeenCalledTimes(1);
    expect(mocks.saveSession).toHaveBeenCalledTimes(1);
  });
  it("exports the exact hydrated pending state and requires native receipt confirmation before reload", async () => {
    const pending = savedSession();
    pending.name = "Unacknowledged name";
    mocks.loadSessionState.mockResolvedValue(
      envelope(pending, { state: "pending", durableRevision: 7 }),
    );
    mocks.chooseRecoveryStorage.mockResolvedValue({
      ...preview,
      hasPendingSave: true,
      requiresDisposition: true,
    });
    mocks.exportProfileReplacementCopy.mockResolvedValue(
      "E:/Protected/pending.json",
    );
    mocks.confirmProfileSwitch.mockResolvedValue(targetBoot);
    render(<App />);
    await openSettings();
    await choose();
    const approval = screen.getByRole("checkbox", {
      name: "I reviewed this profile and its location.",
    });
    await waitFor(() => expect(approval.hasAttribute("disabled")).toBe(false));
    fireEvent.click(approval);
    expect(screen.queryByRole("button", { name: "Switch profile" })).toBeNull();
    const exportCopy = screen.getByRole("button", {
      name: "Save recovery copy and switch",
    });
    await waitFor(() =>
      expect(exportCopy.hasAttribute("disabled")).toBe(false),
    );
    fireEvent.click(exportCopy);
    await waitFor(() =>
      expect(mocks.reloadSelectedProfile).toHaveBeenCalledTimes(1),
    );
    expect(mocks.exportProfileReplacementCopy).toHaveBeenCalledWith(
      preview.token,
    );
    expect(mocks.confirmProfileSwitch).toHaveBeenCalledWith(
      preview.token,
      "recovery-copy",
    );
    expect(mocks.saveSession).not.toHaveBeenCalled();
    expect(
      mocks.reloadSelectedProfile.mock.invocationCallOrder[0],
    ).toBeGreaterThan(mocks.confirmProfileSwitch.mock.invocationCallOrder[0]);
  });
  it("leaves pending settings intact when a profile recovery-copy export is cancelled", async () => {
    const pending = savedSession();
    pending.settings.showNames = false;
    mocks.loadSessionState.mockResolvedValue(
      envelope(pending, { state: "pending", durableRevision: 7 }),
    );
    mocks.chooseRecoveryStorage.mockResolvedValue(preview);
    mocks.exportProfileReplacementCopy.mockResolvedValue(null);
    render(<App />);
    await openSettings();
    await choose();
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "I reviewed this profile and its location.",
      }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Save recovery copy and switch" }),
    );
    await screen.findByText(
      /Recovery copy cancelled.*current profile stays active/,
    );
    expect(mocks.confirmProfileSwitch).not.toHaveBeenCalled();
    expect(mocks.reloadSelectedProfile).not.toHaveBeenCalled();
    await waitFor(() =>
      expect(
        screen
          .getByRole("button", { name: "Keep current profile" })
          .hasAttribute("disabled"),
      ).toBe(false),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Keep current profile" }),
    );
    await waitFor(() =>
      expect(mocks.cancelProfileSwitch).toHaveBeenCalledWith(preview.token),
    );
    expect(
      (
        screen.getByLabelText(
          "Show filenames below comparison images",
        ) as HTMLInputElement
      ).checked,
    ).toBe(false);
    expect(document.querySelector(".save-status")?.textContent).toContain(
      "Save failed",
    );
  });
  it.each(["saved", "discard"] as const)(
    "passes the explicit %s disposition for a pending profile switch",
    async (disposition) => {
      const pending = savedSession();
      pending.name = "Pending profile work";
      mocks.loadSessionState.mockResolvedValue(
        envelope(pending, { state: "pending", durableRevision: 7 }),
      );
      mocks.chooseRecoveryStorage.mockResolvedValue({
        ...preview,
        requiresDisposition: true,
      });
      mocks.saveSession.mockResolvedValue(undefined);
      mocks.refreshProfileSwitch.mockResolvedValue({
        ...preview,
        token: "renewed-profile-token",
      });
      mocks.confirmProfileSwitch.mockResolvedValue(targetBoot);
      render(<App />);
      await openSettings();
      await choose();
      fireEvent.click(
        screen.getByRole("checkbox", {
          name: "I reviewed this profile and its location.",
        }),
      );
      if (disposition === "discard") {
        fireEvent.click(
          screen.getByRole("checkbox", {
            name: /Discard only the current unsaved edits/,
          }),
        );
        fireEvent.click(
          screen.getByRole("button", {
            name: "Switch and discard pending edits",
          }),
        );
      } else {
        fireEvent.click(
          screen.getByRole("button", { name: "Retry save and review profile" }),
        );
        await waitFor(() =>
          expect(mocks.refreshProfileSwitch).toHaveBeenCalledWith(
            preview.token,
            pending,
          ),
        );
        const approval = screen.getByRole("checkbox", {
          name: "I reviewed this profile and its location.",
        });
        await waitFor(() =>
          expect(approval.hasAttribute("disabled")).toBe(false),
        );
        expect((approval as HTMLInputElement).checked).toBe(false);
        fireEvent.click(approval);
        fireEvent.click(screen.getByRole("button", { name: "Switch profile" }));
      }
      await waitFor(() =>
        expect(mocks.confirmProfileSwitch).toHaveBeenCalledWith(
          disposition === "saved" ? "renewed-profile-token" : preview.token,
          disposition,
        ),
      );
      expect(mocks.reloadSelectedProfile).toHaveBeenCalledTimes(1);
      if (disposition === "saved")
        expect(mocks.saveSession).toHaveBeenCalledExactlyOnceWith(pending);
      else expect(mocks.saveSession).not.toHaveBeenCalled();
    },
  );
  it("keeps old controls frozen and offers reload after the native profile switched but navigation failed", async () => {
    mocks.chooseRecoveryStorage.mockResolvedValue(preview);
    mocks.confirmProfileSwitch.mockResolvedValue(targetBoot);
    mocks.reloadSelectedProfile.mockImplementationOnce(() => {
      throw new Error("Navigation failed");
    });
    render(<App />);
    await openSettings();
    await choose();
    await approveSwitch();
    await screen.findByText(
      /profile was switched, but its view could not reload/,
    );
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      true,
    );
    expect(
      screen
        .getByRole("button", { name: "Keep current profile" })
        .hasAttribute("disabled"),
    ).toBe(true);
    fireEvent.click(
      screen.getByRole("button", { name: "Reload selected profile" }),
    );
    expect(mocks.reloadSelectedProfile).toHaveBeenCalledTimes(2);
    expect(mocks.confirmProfileSwitch).toHaveBeenCalledTimes(1);
  });
  it("shows the native selected profile and its session after a fresh renderer mount", async () => {
    const first = render(<App />);
    await screen.findByText("Which image do you prefer?");
    first.unmount();
    const selected = savedSession();
    selected.name = "Reopened selected profile";
    mocks.getBootStatus.mockResolvedValue(targetBoot);
    mocks.rendererContext.mockResolvedValue(targetBoot.contextId);
    mocks.loadSessionState.mockResolvedValue({
      ...envelope(selected),
      contextId: targetBoot.contextId,
      profileGeneration: targetBoot.profileGeneration,
      sessionGeneration: "session-B",
    });
    render(<App />);
    await openSettings();
    expect(screen.getByText(target.displayPath)).toBeTruthy();
    expect(screen.getByText(/profile-B/)).toBeTruthy();
    expect(
      (screen.getByLabelText("Collection name") as HTMLInputElement).value,
    ).toBe(selected.name);
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
  it.each(["contextId", "profileGeneration", "sessionGeneration"])(
    "rejects an unbound hydration %s instead of enabling editing",
    async (field) => {
      mocks.loadSessionState.mockResolvedValue({
        ...envelope(savedSession()),
        [field]: field === "sessionGeneration" ? "" : "mismatched",
      });
      render(<App />);
      await screen.findByText("Your workspace needs recovery");
      expect(
        screen.getByText(/session does not belong to the displayed profile/),
      ).toBeTruthy();
      expect(screen.queryByText("Saved locally")).toBeNull();
      expect(mocks.saveSession).not.toHaveBeenCalled();
    },
  );
});
describe("startup recovery inspection", () => {
  it("excludes dismissed exFAT exports from startup and post-action recovery counts", async () => {
    const pending = {
      id: "unfinished-exfat",
      kind: "copy",
      status: "needs-recovery",
      exfatOutputs: true,
      rows: [],
      errors: [],
      completed: 1,
      manifestPath: null,
      order: 2,
      dependencies: [],
    };
    const dismissed = {
      ...pending,
      id: "dismissed-exfat",
      status: "abandoned",
      order: 1,
    };
    mocks.listOperations.mockResolvedValue([dismissed, pending]);
    mocks.recoverOperation.mockImplementation(async () => {
      const result = { ...pending, status: "abandoned" };
      mocks.listOperations.mockResolvedValue([dismissed, result]);
      return result;
    });
    render(<App />);
    await screen.findByText("1 file operations need recovery.");
    fireEvent.click(screen.getByRole("button", { name: "Inspect recovery" }));
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Keep files and dismiss export",
      }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Confirm keep files and dismiss" }),
    );
    await waitFor(() =>
      expect(mocks.recoverOperation).toHaveBeenCalledExactlyOnceWith(
        "unfinished-exfat",
        "abandon",
      ),
    );
    await waitFor(() =>
      expect(screen.queryByText(/file operations need recovery\./)).toBeNull(),
    );
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
  it("opens trusted pending operations from the restricted shell and reloads after recovery", async () => {
    mocks.getBootStatus.mockResolvedValue({
      ...boot,
      mode: "recovery",
      pendingOperations: 1,
      trustedOperationsAvailable: true,
    });
    const operation = {
      id: "interrupted-rename",
      kind: "rename",
      status: "needs-recovery",
      rows: [],
      errors: [],
      completed: 1,
      manifestPath: null,
      order: 1,
      dependencies: [],
    };
    mocks.listOperations.mockResolvedValue([
      operation,
      { ...operation, id: "old-complete", status: "complete" },
    ]);
    mocks.recoverOperation.mockResolvedValue({
      ...operation,
      status: "complete",
    });
    render(<App />);
    fireEvent.click(await readyRecoveryButton("Recover file operations"));
    await screen.findByText("interrupted-rename");
    expect(screen.queryByText("old-complete")).toBeNull();
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Resume operation" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm resume" }));
    await screen.findByText("Which image do you prefer?");
    expect(mocks.recoverOperation).toHaveBeenCalledWith(
      "interrupted-rename",
      "resume",
    );
    expect(mocks.retryBoot).toHaveBeenCalledTimes(1);
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
  it("does not expose an operation recovery route when trusted journals are unavailable", async () => {
    mocks.getBootStatus.mockResolvedValue({
      ...boot,
      mode: "recovery",
      pendingOperations: 1,
      trustedOperationsAvailable: false,
    });
    render(<App />);
    await screen.findByText("Your workspace needs recovery");
    expect(
      screen.queryByRole("button", { name: "Recover file operations" }),
    ).toBeNull();
    expect(mocks.listOperations).not.toHaveBeenCalled();
  });
  it("freezes app edits while draining multiple queued saves for native close", async () => {
    const session = savedSession();
    session.screen = "sources";
    mocks.loadSessionState.mockResolvedValue({
      ...envelope(session),
      saveStatus: { state: "saved", durableRevision: 3 },
    });
    const acknowledgments: Array<() => void> = [];
    mocks.saveSession.mockImplementation(
      () => new Promise<void>((resolve) => acknowledgments.push(resolve)),
    );
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Settings" }));
    const input = screen.getByLabelText("Collection name");
    fireEvent.change(input, { target: { value: "First edit" } });
    fireEvent.change(input, { target: { value: "Second edit" } });
    await waitFor(() => expect(mocks.saveSession).toHaveBeenCalledTimes(1));
    await act(async () =>
      mocks.subscribeCloseRequested.mock.calls[0][0]({ token: "queued-close" }),
    );
    fireEvent.change(input, { target: { value: "Rejected during close" } });
    expect((input as HTMLInputElement).value).toBe("Second edit");
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    await act(async () => acknowledgments[0]());
    await waitFor(() => expect(mocks.saveSession).toHaveBeenCalledTimes(2));
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    await act(async () => acknowledgments[1]());
    await waitFor(() =>
      expect(mocks.confirmClose).toHaveBeenCalledWith("queued-close", "saved"),
    );
    expect(mocks.saveSession.mock.calls[1][0].name).toBe("Second edit");
  });
  it("does not load or write a session while startup is restricted, then reloads after retry", async () => {
    mocks.getBootStatus.mockResolvedValue({
      ...boot,
      mode: "recovery",
      state: "storage-unavailable",
      issues: [
        { code: "store-unavailable", message: "Reconnect the session drive." },
      ],
    });
    render(<App />);
    await screen.findByText("Your workspace needs recovery");
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
    expect(screen.queryByText("Saved locally")).toBeNull();
    fireEvent.click(await readyRecoveryButton("Retry loading"));
    await screen.findByText("Which image do you prefer?");
    expect(mocks.retryBoot).toHaveBeenCalledTimes(1);
    expect(mocks.loadSessionState).toHaveBeenCalledTimes(1);
  });
  it("restores the validated token with explicit pending consent and reloads the new branch", async () => {
    mocks.getBootStatus
      .mockResolvedValueOnce({
        ...boot,
        mode: "recovery",
        state: "read-only-recovery",
        issues: [],
      })
      .mockResolvedValue(boot);
    mocks.inspectRecoveryCopy.mockResolvedValue(inspection);
    mocks.restoreRecoveryCopy.mockResolvedValue(boot);
    render(<App />);
    fireEvent.click(await readyRecoveryButton("Open recovery copy"));
    await screen.findByText("Recovery copy preview");
    fireEvent.click(
      screen.getByLabelText(
        "Restore the saved state and apply the displayed pending action once after validation.",
      ),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Restore as a new session" }),
    );
    await waitFor(() =>
      expect(mocks.reloadSelectedProfile).toHaveBeenCalledTimes(1),
    );
    expect(mocks.restoreRecoveryCopy).toHaveBeenCalledWith(
      inspection.token,
      true,
      "replacement-authorization",
      "saved",
    );
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
  });
  it("preserves the saved run when a source reconnection is cancelled and when it succeeds", async () => {
    const session = savedSession();
    session.screen = "sources";
    session.report.sources[0].available = false;
    mocks.loadSessionState.mockResolvedValue({
      ...envelope(session),
      saveStatus: { state: "saved", durableRevision: 3 },
    });
    mocks.saveSession.mockResolvedValue(undefined);
    mocks.reconnectSource
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(savedSession().report);
    render(<App />);
    const reconnect = await screen.findByRole("button", {
      name: "Reconnect folder",
    });
    expect(
      screen.getByText(
        "3 ranking images need their source folders reconnected.",
      ),
    ).toBeTruthy();
    await waitFor(() =>
      expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
        false,
      ),
    );
    fireEvent.click(reconnect);
    await screen.findByText(
      "Reconnection cancelled. Your saved ranking is unchanged.",
    );
    expect(mocks.saveSession).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Reconnect folder" }));
    await screen.findByText(
      "Folder identity checked. Unchanged images can resume their saved ranking.",
    );
    expect(mocks.saveSession.mock.calls[0][0].run).toEqual(session.run);
    expect(
      screen.queryByRole("button", { name: "Reconnect folder" }),
    ).toBeNull();
  });
  it("offers reload recovery after a branch restore commits but renderer navigation fails", async () => {
    mocks.getBootStatus.mockResolvedValue({ ...boot, mode: "recovery" });
    mocks.inspectRecoveryCopy.mockResolvedValue({
      ...inspection,
      pendingActions: 0,
    });
    mocks.restoreRecoveryCopy.mockResolvedValue({
      ...boot,
      contextId: "new-branch-context",
    });
    mocks.reloadSelectedProfile.mockImplementationOnce(() => {
      throw new Error("Navigation unavailable");
    });
    render(<App />);
    fireEvent.click(await readyRecoveryButton("Open recovery copy"));
    fireEvent.click(
      await screen.findByRole("button", { name: "Restore as a new session" }),
    );
    await screen.findByRole("dialog", { name: "Reload restored workspace" });
    expect(
      screen.getByText(/Reload failed: Error: Navigation unavailable/),
    ).toBeTruthy();
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      true,
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Reload selected profile" }),
    );
    expect(mocks.reloadSelectedProfile).toHaveBeenCalledTimes(2);
    expect(mocks.restoreRecoveryCopy).toHaveBeenCalledTimes(1);
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
  it("keeps voting disabled when a native preview returns a mismatched request or revision", async () => {
    mocks.requestPreview.mockResolvedValue({
      status: "ready",
      path: "preview.png",
      requestId: "old-request",
      leaseId: "old-lease",
      contentRevision: "wrong-revision",
      assetToken: "wrong-asset",
      width: 10,
      height: 10,
      scale: 1,
      pipelineVersion: "v1",
    });
    render(<App />);
    await screen.findAllByText("Preview unavailable");
    expect(
      screen
        .getByRole("button", { name: /Prefer left/ })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(mocks.saveSession).not.toHaveBeenCalled();
    const request = mocks.requestPreview.mock.calls[0];
    expect(request[2]).toBe(
      savedSession().report.images.find((image) => image.id === request[0])
        ?.contentRevision,
    );
    expect(request[3]).toBeTypeOf("string");
  });
  it("explains missing journals/backups and requires review of captured pending actions before restore", async () => {
    const onOpen = vi.fn().mockResolvedValue(inspection),
      onRestore = vi.fn().mockResolvedValue(boot),
      onRestored = vi.fn();
    render(
      <StartupRecovery
        status={{
          mode: "recovery",
          issues: [
            {
              code: "journal-unavailable",
              message: "A trusted journal cannot be read.",
            },
          ],
          canRestoreBackup: true,
        }}
        failure=""
        onRetry={async () => {}}
        onOpen={onOpen}
        onRestore={onRestore}
        onRestored={onRestored}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Open recovery copy" }));
    await screen.findByText("Recovery copy preview");
    expect(screen.getByText("Trusted file journals unavailable")).toBeTruthy();
    expect(screen.getByText("Recovery backup files unavailable")).toBeTruthy();
    const restore = screen.getByRole("button", {
      name: "Restore as a new session",
    });
    expect(restore.hasAttribute("disabled")).toBe(true);
    expect(onRestore).not.toHaveBeenCalled();
    fireEvent.click(
      screen.getByLabelText(
        "Restore the saved state and apply the displayed pending action once after validation.",
      ),
    );
    fireEvent.click(restore);
    await waitFor(() =>
      expect(onRestore).toHaveBeenCalledWith("native-validated-token", true),
    );
    expect(onRestored).toHaveBeenCalledWith(boot);
  });
  it("keeps an unsupported recovery copy read-only", async () => {
    render(
      <StartupRecovery
        status={boot}
        failure=""
        onRetry={async () => {}}
        onOpen={async () => ({
          ...inspection,
          restorable: false,
          warnings: ["This copy uses a newer version."],
        })}
        onRestore={async () => boot}
        onRestored={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Open recovery copy" }));
    await screen.findByText("Read-only inspection");
    expect(
      screen
        .getByRole("button", { name: "Restore as a new session" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(screen.getByText("This copy uses a newer version.")).toBeTruthy();
  });
  it("handles picker cancellation and retry failures without inventing an empty recovered session", async () => {
    render(
      <StartupRecovery
        status={boot}
        failure=""
        onRetry={async () => {
          throw new Error("Store still unavailable");
        }}
        onOpen={async () => null}
        onRestored={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Open recovery copy" }));
    await screen.findByText(
      "No recovery copy selected. Your current data is unchanged.",
    );
    expect(screen.queryByText("Recovery copy preview")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Retry loading" }));
    await screen.findByText("Error: Store still unavailable");
  });
});

describe("shared current-session authorization for recovery replacement", () => {
  const pendingReplacement = {
    token: "replacement-current",
    subjectToken: inspection.token,
    subjectKind: "fresh",
    currentSaveState: "pending",
    hasCurrentSession: true,
    requiresDisposition: true,
    rendererChangesUnadmitted: false,
    pendingLayers: [
      {
        kind: "pending-event",
        id: "current-event",
        summary: "Current final comparison",
      },
      {
        kind: "pending-proposal",
        id: "current-proposal",
        summary: "Current manual picks",
      },
    ],
    warnings: [],
  };
  async function inspectFromSettings() {
    fireEvent.click(await screen.findByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("button", { name: "Open recovery copy" }));
    await screen.findByText("Open a session recovery copy");
    fireEvent.click(await readyRecoveryButton("Open recovery copy"));
    await screen.findByText("Recovery copy preview");
    fireEvent.click(
      screen.getByLabelText(
        "Restore the saved state and apply the displayed pending action once after validation.",
      ),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Restore as a new session" }),
    );
    return screen.findByRole("button", {
      name: "Save current recovery copy and restore",
    });
  }
  it("inspects after a rejected hydrated queue, freezes the exact current state, and exports native admitted layers", async () => {
    const session = savedSession();
    session.settings.showNames = false;
    mocks.loadSessionState.mockResolvedValue(
      envelope(session, {
        state: "pending",
        durableRevision: 3,
        pendingEventId: "current-event",
        message: "Current edits need save retry",
      }),
    );
    mocks.inspectRecoveryCopy.mockResolvedValue(inspection);
    mocks.prepareRecoveryReplacement.mockResolvedValue(pendingReplacement);
    mocks.restoreRecoveryCopy.mockResolvedValue(boot);
    render(<App />);
    await screen.findByText("Save failed");
    fireEvent.click(await inspectFromSettings());
    await waitFor(() =>
      expect(mocks.reloadSelectedProfile).toHaveBeenCalledTimes(1),
    );
    expect(mocks.prepareRecoveryReplacement).toHaveBeenCalledWith(
      { kind: "fresh", token: inspection.token, acceptPending: true },
      session,
    );
    expect(mocks.exportRecoveryReplacementCopy).toHaveBeenCalledExactlyOnceWith(
      "replacement-current",
    );
    expect(mocks.exportSessionCopy).not.toHaveBeenCalled();
    expect(mocks.restoreRecoveryCopy).toHaveBeenCalledExactlyOnceWith(
      inspection.token,
      true,
      "replacement-current",
      "recovery-copy",
    );
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
  it("returns from cancelled current export to the same inspected subject and failed current save", async () => {
    const session = savedSession();
    mocks.loadSessionState.mockResolvedValue(
      envelope(session, {
        state: "pending",
        durableRevision: 3,
        pendingEventId: "current-event",
      }),
    );
    mocks.inspectRecoveryCopy.mockResolvedValue(inspection);
    mocks.prepareRecoveryReplacement.mockResolvedValue(pendingReplacement);
    mocks.exportRecoveryReplacementCopy.mockResolvedValue(null);
    render(<App />);
    await screen.findByText("Save failed");
    fireEvent.click(await inspectFromSettings());
    await screen.findByText(/Recovery copy cancelled/);
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      true,
    );
    expect(mocks.restoreRecoveryCopy).not.toHaveBeenCalled();
    fireEvent.click(
      screen.getByRole("button", { name: "Keep current session" }),
    );
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", {
          name: "Preserve current work before restoring",
        }),
      ).toBeNull(),
    );
    expect(screen.getByText("Recovery copy preview")).toBeTruthy();
    expect(screen.getByText("Save failed")).toBeTruthy();
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      false,
    );
    expect(mocks.inspectRecoveryCopy).toHaveBeenCalledTimes(1);
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
  it("omits any current renderer snapshot at restricted startup instead of exporting an empty placeholder", async () => {
    mocks.getBootStatus.mockResolvedValue({
      ...boot,
      mode: "recovery",
      state: "storage-unavailable",
    });
    mocks.inspectRecoveryCopy.mockResolvedValue({
      ...inspection,
      pendingActions: 0,
    });
    mocks.restoreRecoveryCopy.mockResolvedValue(boot);
    render(<App />);
    fireEvent.click(await readyRecoveryButton("Open recovery copy"));
    fireEvent.click(
      await screen.findByRole("button", { name: "Restore as a new session" }),
    );
    await waitFor(() =>
      expect(mocks.reloadSelectedProfile).toHaveBeenCalledTimes(1),
    );
    expect(mocks.prepareRecoveryReplacement).toHaveBeenCalledExactlyOnceWith(
      { kind: "fresh", token: inspection.token, acceptPending: false },
      undefined,
    );
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
    expect(mocks.exportSessionCopy).not.toHaveBeenCalled();
    expect(mocks.exportRecoveryReplacementCopy).not.toHaveBeenCalled();
  });
  it("routes retained resume through the same replacement gate without overriding its original imported choice", async () => {
    mocks.getBootStatus.mockResolvedValue({ ...boot, mode: "recovery" });
    mocks.recoveryStorageStatus.mockResolvedValue({
      selectedRequestId: null,
      selectedBranchId: null,
      activationState: "original",
      usedBranches: 1,
      usedBytes: 100,
      limits: { branches: 8, bytes: 2147483648, requests: 1024 },
      requests: [
        {
          requestId: "retained-request",
          branchId: "retained-branch",
          state: "staged",
          pendingActions: 1,
          acceptedPending: true,
          bytes: 100,
          canResume: true,
          canCleanup: false,
          reason: null,
        },
      ],
      warnings: [],
    });
    mocks.prepareRecoveryResume.mockResolvedValue({
      token: "retained-token",
      requestId: "retained-request",
      branchId: "retained-branch",
      inspection,
      acceptedPending: true,
      action: "resume-bootstrap",
      warnings: [],
    });
    mocks.prepareRecoveryReplacement.mockResolvedValue({
      ...pendingReplacement,
      subjectToken: "retained-token",
      subjectKind: "resume",
    });
    mocks.confirmRecoveryResume.mockResolvedValue(boot);
    render(<App />);
    fireEvent.click(await readyRecoveryButton("Preview resume"));
    fireEvent.click(
      await screen.findByRole("button", { name: "Confirm resume" }),
    );
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Save current recovery copy and restore",
      }),
    );
    await waitFor(() =>
      expect(mocks.reloadSelectedProfile).toHaveBeenCalledTimes(1),
    );
    expect(mocks.prepareRecoveryReplacement).toHaveBeenCalledExactlyOnceWith(
      { kind: "resume", token: "retained-token" },
      undefined,
    );
    expect(mocks.confirmRecoveryResume).toHaveBeenCalledExactlyOnceWith(
      "retained-token",
      "replacement-current",
      "recovery-copy",
    );
    expect(mocks.restoreRecoveryCopy).not.toHaveBeenCalled();
  });
});

describe("owned startup loading", () => {
  const startup = {
    id: "startup-job",
    contextId: boot.contextId,
    profileGeneration: boot.profileGeneration,
    kind: "startup-load",
    phase: "loading-session",
    state: "running",
    completed: 0,
    total: null,
    cancelRequested: false,
    message: null,
  };
  it("does not hydrate or claim recovery failure while native startup is still loading", async () => {
    mocks.getBootStatus.mockResolvedValue({
      ...boot,
      mode: "recovery",
      state: "loading",
    });
    mocks.recoveryWorkStatus.mockResolvedValue({
      active: startup,
      recent: [],
      limit: 1,
    });
    render(<App />);
    await screen.findByRole("heading", { name: "Opening your workspace" });
    expect(screen.queryByText("Your workspace needs recovery")).toBeNull();
    expect(screen.queryByText("Saved locally")).toBeNull();
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
    expect(mocks.retryBoot).not.toHaveBeenCalled();
    expect(
      screen.getByRole("button", { name: "Cancel recovery work" }),
    ).toBeTruthy();
  });
  it("waits for exact startup terminal even after boot publishes ready, then hydrates without a write", async () => {
    mocks.getBootStatus.mockResolvedValue({
      ...boot,
      mode: "recovery",
      state: "loading",
    });
    mocks.recoveryWorkStatus.mockResolvedValue({
      active: startup,
      recent: [],
      limit: 1,
    });
    render(<App />);
    await screen.findByRole("heading", { name: "Opening your workspace" });
    mocks.getBootStatus.mockResolvedValue(boot);
    mocks.recoveryWorkStatus.mockResolvedValue({
      active: null,
      recent: [],
      limit: 1,
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Refresh workspace status" }),
    );
    await act(async () => {});
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    mocks.recoveryWorkStatus.mockResolvedValue({
      active: null,
      recent: [{ ...startup, state: "completed" }],
      limit: 1,
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Refresh workspace status" }),
    );
    await screen.findByText("Which image do you prefer?");
    expect(mocks.loadSessionState).toHaveBeenCalledTimes(1);
    expect(mocks.saveSession).not.toHaveBeenCalled();
    expect(mocks.retryBoot).not.toHaveBeenCalled();
  });
  it("exposes cancelled startup as restricted retry after native worker terminal without automatic restart", async () => {
    mocks.getBootStatus.mockResolvedValue({
      ...boot,
      mode: "recovery",
      state: "load-cancelled",
      issues: [
        {
          code: "RECOVERY_CANCELLED",
          message: "Loading stopped before publication.",
        },
      ],
    });
    mocks.recoveryWorkStatus.mockResolvedValue({
      active: null,
      recent: [{ ...startup, state: "cancelled" }],
      limit: 1,
    });
    render(<App />);
    await screen.findByText("Your workspace needs recovery");
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    expect(mocks.retryBoot).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
});

describe("shared protection worker editing guard", () => {
  it("offers maintenance at unhydrated restricted startup without a placeholder session", async () => {
    mocks.getBootStatus.mockResolvedValue({
      ...boot,
      mode: "recovery",
      state: "storage-unavailable",
    });
    mocks.protectionMaintenanceStatus.mockResolvedValue({
      registry: null,
      result: null,
      active: false,
      fresh: true,
      error: "Registry unavailable",
    });
    render(<App />);
    const open = await screen.findByRole("button", {
      name: "Review protection storage maintenance",
    });
    await waitFor(() => expect(open.hasAttribute("disabled")).toBe(false));
    fireEvent.click(open);
    await screen.findByRole("dialog", {
      name: "Protection storage maintenance",
    });
    const check = screen.getByRole("button", {
      name: "Check retained maintenance request",
    });
    await waitFor(() => expect(check.hasAttribute("disabled")).toBe(false));
    fireEvent.click(check);
    await screen.findByText("Error: Registry unavailable");
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
    expect(mocks.protectionMaintenanceStatus).toHaveBeenCalledTimes(1);
  });
  it("discovers a retired maintenance worker after renderer mount and keeps edits frozen after closing its dialog", async () => {
    const job = {
      id: "retired-registry-job",
      contextId: "prior-renderer",
      profileGeneration: "native-profile",
      kind: "registry-maintenance",
      phase: "finishing-maintenance",
      state: "running",
      completed: 0,
      total: null,
      cancelRequested: false,
      message: null,
    };
    mocks.recoveryWorkStatus.mockResolvedValue({
      active: job,
      recent: [],
      limit: 1,
    });
    render(<App />);
    await screen.findByText("Which image do you prefer?");
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      true,
    );
    fireEvent.click(
      screen.getByRole("button", {
        name: "Open protection storage maintenance",
      }),
    );
    await screen.findByRole("dialog", {
      name: "Protection storage maintenance",
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Close maintenance view" }),
    );
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      true,
    );
    mocks.recoveryWorkStatus.mockResolvedValue({
      active: null,
      recent: [],
      limit: 1,
    });
    const refresh = await screen.findByRole("button", {
      name: "Refresh recovery work status",
    });
    await waitFor(() => expect(refresh.hasAttribute("disabled")).toBe(false));
    fireEvent.click(refresh);
    await act(async () => {});
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      true,
    );
    mocks.recoveryWorkStatus.mockResolvedValue({
      active: null,
      recent: [{ ...job, state: "completed" }],
      limit: 1,
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Refresh recovery work status" }),
    );
    await waitFor(() =>
      expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
        false,
      ),
    );
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
});

describe("native retained file result recovery", () => {
  const restrictedBoot = (code = "FILE_OPERATION_PUBLICATION_REQUIRED") => ({
    ...boot,
    mode: "recovery",
    state: "file-operation-unresolved",
    trustedOperationsAvailable: false,
    issues: [{ code, message: `${code}: native retained worker` }],
  });
  it("discovers publication recovery after renderer mount without hydrating or repeating file work", async () => {
    mocks.getBootStatus.mockResolvedValue(restrictedBoot());
    render(<App />);
    const retry = await screen.findByRole("button", {
      name: "Retry file result publication",
    });
    expect(mocks.retryOperationPublication).not.toHaveBeenCalled();
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      true,
    );
    mocks.retryOperationPublication.mockImplementation(async () => {
      mocks.getBootStatus.mockResolvedValue(boot);
    });
    fireEvent.click(retry);
    await screen.findByText("Which image do you prefer?");
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Retry file result publication" }),
      ).toBeNull(),
    );
    expect(mocks.retryOperationPublication).toHaveBeenCalledExactlyOnceWith();
    expect(mocks.recoverOperation).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
    expect(mocks.loadSessionState).toHaveBeenCalledOnce();
  });
  it("retains failed publication state and blocks duplicate clicks until command delivery", async () => {
    mocks.getBootStatus.mockResolvedValue(restrictedBoot());
    let reject!: (reason: unknown) => void;
    mocks.retryOperationPublication.mockImplementation(
      () =>
        new Promise((_done, fail) => {
          reject = fail;
        }),
    );
    render(<App />);
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Retry file result publication",
      }),
    );
    await waitFor(() =>
      expect(mocks.retryOperationPublication).toHaveBeenCalledOnce(),
    );
    const pending = screen.getByRole("button", {
      name: "Publishing retained result…",
    });
    expect(pending.hasAttribute("disabled")).toBe(true);
    fireEvent.click(pending);
    await act(async () => reject(new Error("Publication still blocked")));
    await screen.findByText("Error: Publication still blocked");
    expect(mocks.retryOperationPublication).toHaveBeenCalledOnce();
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      true,
    );
  });
  it("never offers publication retry for an unverified native panic, including a later generic restriction", async () => {
    mocks.getBootStatus.mockResolvedValue(
      restrictedBoot("FILE_OPERATION_RESTART_REQUIRED"),
    );
    render(<App />);
    await screen.findByText(
      /Reloading this window does not restart the application/,
    );
    expect(
      screen.queryByRole("button", { name: "Retry file result publication" }),
    ).toBeNull();
    const listener = mocks.onOperationRestriction.mock.calls[0][0];
    act(() =>
      listener({
        kind: "publication",
        message:
          "FILE_OPERATION_PUBLICATION_REQUIRED: later rejected operation",
      }),
    );
    expect(
      screen.queryByRole("button", { name: "Retry file result publication" }),
    ).toBeNull();
    expect(mocks.retryOperationPublication).not.toHaveBeenCalled();
    expect(mocks.reloadSelectedProfile).not.toHaveBeenCalled();
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
  });
  it("offers preservation review before closing an unverified file worker for restart", async () => {
    mocks.getBootStatus.mockResolvedValue(
      restrictedBoot("FILE_OPERATION_RESTART_REQUIRED"),
    );
    render(<App />);
    await screen.findByText(
      /Reloading this window does not restart the application/,
    );
    expect(
      screen
        .getByRole("button", {
          name: "Preserve work and review restart",
        })
        .hasAttribute("disabled"),
    ).toBe(false);
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      true,
    );
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    expect(mocks.retryOperationPublication).not.toHaveBeenCalled();
  });
  async function reviewRestart() {
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Preserve work and review restart",
      }),
    );
    await screen.findByText("Verified saved revision");
  }
  it("reviews an unhydrated panic without a placeholder, requires native copy, then requests only restart close", async () => {
    mocks.getBootStatus.mockResolvedValue(
      restrictedBoot("FILE_OPERATION_RESTART_REQUIRED"),
    );
    render(<App />);
    await reviewRestart();
    expect(mocks.prepareOperationRestart).toHaveBeenCalledExactlyOnceWith(
      undefined,
    );
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    expect(
      screen
        .getByRole("button", { name: "Close for restart" })
        .hasAttribute("disabled"),
    ).toBe(true);
    fireEvent.click(
      screen.getByRole("button", { name: "Save restart recovery copy" }),
    );
    await screen.findByText(
      "Native recovery-copy verification is complete for this restart review.",
    );
    expect(mocks.exportOperationRestart).toHaveBeenCalledExactlyOnceWith(
      "restart-token-A",
    );
    fireEvent.click(screen.getByRole("button", { name: "Close for restart" }));
    await screen.findByRole("button", { name: "Retry close for restart" });
    expect(mocks.confirmOperationRestart).toHaveBeenCalledExactlyOnceWith(
      "restart-token-A",
      undefined,
    );
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    expect(mocks.exportSessionCopy).not.toHaveBeenCalled();
    expect(mocks.reloadSelectedProfile).not.toHaveBeenCalled();
    expect(mocks.retryOperationPublication).not.toHaveBeenCalled();
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      true,
    );
  });
  it("preserves the exact hydrated pending UI after a failed queue and freezes the same expectation through close", async () => {
    const pending = savedSession();
    pending.settings.showNames = false;
    mocks.loadSessionState.mockResolvedValue(
      envelope(pending, {
        state: "pending",
        durableRevision: 3,
        pendingEventId: "unacknowledged",
      }),
    );
    render(<App />);
    await screen.findByText("Save failed");
    act(() =>
      mocks.onOperationRestriction.mock.calls[0][0]({
        kind: "restart",
        message: "FILE_OPERATION_RESTART_REQUIRED: caught panic",
      }),
    );
    await reviewRestart();
    const expected = mocks.prepareOperationRestart.mock.calls[0][0];
    expect(expected.settings.showNames).toBe(false);
    expect(expected.run).toEqual(pending.run);
    expect(expected).not.toBe(pending);
    await screen.findByText(/save queue did not finish durably/i);
    fireEvent.click(
      screen.getByRole("button", { name: "Save restart recovery copy" }),
    );
    await screen.findByText(
      "Native recovery-copy verification is complete for this restart review.",
    );
    fireEvent.click(screen.getByRole("button", { name: "Close for restart" }));
    await waitFor(() =>
      expect(mocks.confirmOperationRestart).toHaveBeenCalledExactlyOnceWith(
        "restart-token-A",
        expected,
      ),
    );
    expect(mocks.saveSession).not.toHaveBeenCalled();
    expect(mocks.exportSessionCopy).not.toHaveBeenCalled();
  });
  it("keeps picker cancellation and an unverified copy response from authorizing close", async () => {
    mocks.getBootStatus.mockResolvedValue(
      restrictedBoot("FILE_OPERATION_RESTART_REQUIRED"),
    );
    mocks.exportOperationRestart
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce({
        token: "restart-token-A",
        requestId: "panicked-file-worker-A",
        savedRevision: 3,
        pendingActions: 2,
        rendererProposal: false,
        copyReady: false,
        copyPath: "C:/Unverified.json",
        warnings: [],
      });
    render(<App />);
    await reviewRestart();
    fireEvent.click(
      screen.getByRole("button", { name: "Save restart recovery copy" }),
    );
    await screen.findByText(/file picker was cancelled/);
    expect(
      screen
        .getByRole("button", { name: "Close for restart" })
        .hasAttribute("disabled"),
    ).toBe(true);
    fireEvent.click(
      screen.getByRole("button", { name: "Save restart recovery copy" }),
    );
    await screen.findByText(/has not received native verification/);
    expect(
      screen
        .getByRole("button", { name: "Close for restart" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(mocks.confirmOperationRestart).not.toHaveBeenCalled();
  });
  it("rejects a copy response for another token or retained worker without adopting its claimed verification", async () => {
    mocks.getBootStatus.mockResolvedValue(
      restrictedBoot("FILE_OPERATION_RESTART_REQUIRED"),
    );
    mocks.exportOperationRestart.mockResolvedValue({
      token: "other-token",
      requestId: "other-worker",
      savedRevision: 3,
      pendingActions: 2,
      rendererProposal: false,
      copyReady: true,
      copyPath: "C:/Other.json",
      warnings: [],
    });
    render(<App />);
    await reviewRestart();
    fireEvent.click(
      screen.getByRole("button", { name: "Save restart recovery copy" }),
    );
    await screen.findByText(
      /copy response belongs to a different restart review/,
    );
    expect(
      screen
        .getByRole("button", { name: "Close for restart" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(
      screen.queryByText(/Recovery copy location: C:\/Other.json/),
    ).toBeNull();
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel restart review" }),
    );
    await screen.findByRole("button", {
      name: "Preserve work and review restart",
    });
    expect(mocks.cancelOperationRestart).toHaveBeenCalledExactlyOnceWith(
      "restart-token-A",
    );
  });
  it("retains the exact export token across a lost response and never falls back to ordinary export", async () => {
    mocks.getBootStatus.mockResolvedValue(
      restrictedBoot("FILE_OPERATION_RESTART_REQUIRED"),
    );
    mocks.exportOperationRestart.mockRejectedValueOnce(
      new Error("Transport lost after copy publication"),
    );
    render(<App />);
    await reviewRestart();
    fireEvent.click(
      screen.getByRole("button", { name: "Save restart recovery copy" }),
    );
    await screen.findByText("Error: Transport lost after copy publication");
    fireEvent.click(
      screen.getByRole("button", { name: "Save restart recovery copy" }),
    );
    await screen.findByText(
      "Native recovery-copy verification is complete for this restart review.",
    );
    expect(mocks.exportOperationRestart.mock.calls).toEqual([
      ["restart-token-A"],
      ["restart-token-A"],
    ]);
    expect(mocks.prepareOperationRestart).toHaveBeenCalledOnce();
    expect(mocks.exportSessionCopy).not.toHaveBeenCalled();
  });
  it("keeps an unknown close outcome on the exact request and disables new approval or cancellation", async () => {
    mocks.getBootStatus.mockResolvedValue(
      restrictedBoot("FILE_OPERATION_RESTART_REQUIRED"),
    );
    mocks.confirmOperationRestart.mockRejectedValueOnce(
      new Error("Window close response lost"),
    );
    render(<App />);
    await reviewRestart();
    fireEvent.click(
      screen.getByRole("button", { name: "Save restart recovery copy" }),
    );
    await screen.findByText(
      "Native recovery-copy verification is complete for this restart review.",
    );
    fireEvent.click(screen.getByRole("button", { name: "Close for restart" }));
    await screen.findByText(/close outcome is unconfirmed/);
    expect(
      screen
        .getByRole("button", { name: "Cancel restart review" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(
      screen
        .getByRole("button", { name: "Save restart recovery copy" })
        .hasAttribute("disabled"),
    ).toBe(true);
    fireEvent.click(
      screen.getByRole("button", { name: "Retry close for restart" }),
    );
    await screen.findByText(/Native close has been requested/);
    expect(mocks.confirmOperationRestart.mock.calls).toEqual([
      ["restart-token-A", undefined],
      ["restart-token-A", undefined],
    ]);
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    expect(mocks.reloadSelectedProfile).not.toHaveBeenCalled();
  });
  it("requires a fresh review and fresh copy after native expectation changes", async () => {
    mocks.getBootStatus.mockResolvedValue(
      restrictedBoot("FILE_OPERATION_RESTART_REQUIRED"),
    );
    mocks.confirmOperationRestart.mockRejectedValueOnce(
      new Error("OPERATION_RESTART_EXPECTATION_CHANGED: snapshot changed"),
    );
    render(<App />);
    await reviewRestart();
    fireEvent.click(
      screen.getByRole("button", { name: "Save restart recovery copy" }),
    );
    await screen.findByText(
      "Native recovery-copy verification is complete for this restart review.",
    );
    fireEvent.click(screen.getByRole("button", { name: "Close for restart" }));
    await screen.findByRole("button", { name: "Review retained work again" });
    expect(
      screen
        .getByRole("button", { name: "Close for restart" })
        .hasAttribute("disabled"),
    ).toBe(true);
    mocks.prepareOperationRestart.mockResolvedValue({
      token: "restart-token-B",
      requestId: "panicked-file-worker-A",
      savedRevision: 3,
      pendingActions: 2,
      rendererProposal: false,
      copyReady: false,
      copyPath: null,
      warnings: [],
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Review retained work again" }),
    );
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Review retained work again" }),
      ).toBeNull(),
    );
    expect(
      screen
        .getByRole("button", { name: "Close for restart" })
        .hasAttribute("disabled"),
    ).toBe(true);
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel restart review" }),
    );
    await screen.findByRole("button", {
      name: "Preserve work and review restart",
    });
    expect(mocks.cancelOperationRestart).toHaveBeenCalledExactlyOnceWith(
      "restart-token-B",
    );
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      true,
    );
  });
  it("retains failed native cancel and keeps Help responsive while restart export is pending", async () => {
    mocks.getBootStatus.mockResolvedValue(
      restrictedBoot("FILE_OPERATION_RESTART_REQUIRED"),
    );
    let finish!: (value: null) => void;
    mocks.exportOperationRestart.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    render(<App />);
    await reviewRestart();
    fireEvent.click(
      screen.getByRole("button", { name: "Save restart recovery copy" }),
    );
    await waitFor(() =>
      expect(mocks.exportOperationRestart).toHaveBeenCalledOnce(),
    );
    const cancel = screen.getByRole("button", {
      name: "Cancel restart review",
    });
    expect(cancel.hasAttribute("disabled")).toBe(true);
    fireEvent.click(cancel);
    expect(mocks.cancelOperationRestart).not.toHaveBeenCalled();
    const recoveryDialog = screen.getByRole("dialog", {
      name: "File operation needs recovery",
    });
    const help = within(recoveryDialog).getByRole("button", {
      name: "Help and local diagnostics",
    });
    expect(help.hasAttribute("disabled")).toBe(false);
    fireEvent.click(help);
    const helpDialog = await screen.findByRole("dialog", {
      name: "Help and local support",
    });
    fireEvent.click(
      within(helpDialog).getByRole("button", { name: "Close dialog" }),
    );
    await act(async () => finish(null));
    mocks.cancelOperationRestart.mockRejectedValueOnce(
      new Error("OPERATION_RESTART_BUSY: retained native transaction"),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel restart review" }),
    );
    await screen.findByText(
      "Error: OPERATION_RESTART_BUSY: retained native transaction",
    );
    expect(screen.getByText("Verified saved revision")).toBeTruthy();
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel restart review" }),
    );
    await screen.findByText(/Restart review cancelled/);
    expect(mocks.cancelOperationRestart.mock.calls).toEqual([
      ["restart-token-A"],
      ["restart-token-A"],
    ]);
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      true,
    );
  });
  it("does not persist local restart authorization through renderer remount or automatically prepare", async () => {
    mocks.getBootStatus.mockResolvedValue(
      restrictedBoot("FILE_OPERATION_RESTART_REQUIRED"),
    );
    const view = render(<App />);
    await reviewRestart();
    view.unmount();
    render(<App />);
    await screen.findByRole("button", {
      name: "Preserve work and review restart",
    });
    expect(mocks.prepareOperationRestart).toHaveBeenCalledOnce();
    expect(
      screen.queryByRole("button", { name: "Close for restart" }),
    ).toBeNull();
    expect(mocks.confirmOperationRestart).not.toHaveBeenCalled();
  });
  it("does not infer file publication authority from an unrelated boot issue", async () => {
    mocks.getBootStatus.mockResolvedValue(restrictedBoot("IMAGE_WORK_BUSY"));
    render(<App />);
    await screen.findByText("Your workspace needs recovery");
    expect(
      screen.queryByRole("button", { name: "Retry file result publication" }),
    ).toBeNull();
    expect(mocks.retryOperationPublication).not.toHaveBeenCalled();
  });
  it("rechecks a changed native panic before invoking a previously visible publication retry", async () => {
    mocks.getBootStatus.mockResolvedValue(restrictedBoot());
    render(<App />);
    const retry = await screen.findByRole("button", {
      name: "Retry file result publication",
    });
    mocks.getBootStatus.mockResolvedValue(
      restrictedBoot("FILE_OPERATION_RESTART_REQUIRED"),
    );
    fireEvent.click(retry);
    await screen.findByText(
      /Reloading this window does not restart the application/,
    );
    expect(mocks.retryOperationPublication).not.toHaveBeenCalled();
  });
  it("uses fresh native boot to settle an already published lost response without redoing the operation", async () => {
    mocks.getBootStatus.mockResolvedValue(restrictedBoot());
    render(<App />);
    const retry = await screen.findByRole("button", {
      name: "Retry file result publication",
    });
    mocks.getBootStatus.mockResolvedValue(boot);
    fireEvent.click(retry);
    await screen.findByText("Which image do you prefer?");
    expect(mocks.retryOperationPublication).not.toHaveBeenCalled();
    expect(mocks.recoverOperation).not.toHaveBeenCalled();
  });
  it("explicitly resumes the native recorded profile and reloads only after known confirmation", async () => {
    const pendingProfileSwitch = {
      token: "persisted-profile-intent",
      disposition: "recovery-copy",
      profile: { ...boot.profile, displayPath: "D:/Recorded/Profile" },
      warnings: [],
    };
    mocks.getBootStatus.mockResolvedValue({
      ...boot,
      mode: "recovery",
      state: "profile-selection-unresolved",
      pendingProfileSwitch,
    });
    mocks.confirmProfileSwitch.mockResolvedValue({
      ...boot,
      contextId: "native-selected-context",
      profile: pendingProfileSwitch.profile,
    });
    render(<App />);
    const resume = await screen.findByRole("button", {
      name: "Resume selected profile",
    });
    await waitFor(() => expect(resume.hasAttribute("disabled")).toBe(false));
    expect(mocks.confirmProfileSwitch).not.toHaveBeenCalled();
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    fireEvent.click(resume);
    await waitFor(() =>
      expect(mocks.reloadSelectedProfile).toHaveBeenCalledOnce(),
    );
    expect(mocks.confirmProfileSwitch).toHaveBeenCalledExactlyOnceWith(
      "persisted-profile-intent",
      "recovery-copy",
    );
    expect(mocks.chooseRecoveryStorage).not.toHaveBeenCalled();
    expect(mocks.openRecoveryProfile).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
});

it("keeps publication recovery frozen when boot readiness or current-context proof is unavailable", async () => {
  const issue = {
    code: "FILE_OPERATION_PUBLICATION_REQUIRED",
    message: "Retained publication",
  };
  mocks.getBootStatus.mockResolvedValue({
    ...boot,
    mode: "recovery",
    state: "file-operation-unresolved",
    issues: [issue],
  });
  render(<App />);
  const retry = await screen.findByRole("button", {
    name: "Retry file result publication",
  });
  mocks.getBootStatus.mockResolvedValue({
    ...boot,
    mode: "recovery",
    state: "storage-unavailable",
    issues: [],
  });
  fireEvent.click(retry);
  await screen.findByText(/Native workspace readiness is still unconfirmed/);
  expect(mocks.loadSessionState).not.toHaveBeenCalled();
  expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
    true,
  );
  mocks.getBootStatus.mockResolvedValue({
    ...boot,
    contextId: "unrelated-context",
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Retry file result publication" }),
  );
  await screen.findByText(/STALE_CONTEXT: reload the selected workspace/);
  expect(mocks.retryOperationPublication).not.toHaveBeenCalled();
  expect(mocks.loadSessionState).not.toHaveBeenCalled();
});
it("freezes live edits on a native publication error while keeping independent Help available", async () => {
  render(<App />);
  await screen.findByText("Which image do you prefer?");
  await waitFor(() =>
    expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
      false,
    ),
  );
  const listener = mocks.onOperationRestriction.mock.calls[0][0];
  act(() =>
    listener({
      kind: "publication",
      message: "FILE_OPERATION_PUBLICATION_REQUIRED: native worker retained",
    }),
  );
  await screen.findByRole("button", { name: "Retry file result publication" });
  expect(document.querySelector(".app-shell")?.hasAttribute("inert")).toBe(
    true,
  );
  fireEvent.keyDown(window, { key: "ArrowLeft" });
  expect(mocks.saveSession).not.toHaveBeenCalled();
  fireEvent.click(
    screen.getByRole("button", { name: "Help and local diagnostics" }),
  );
  expect(await screen.findByRole("dialog", { name: /Help/ })).toBeTruthy();
  expect(mocks.retryOperationPublication).not.toHaveBeenCalled();
});
