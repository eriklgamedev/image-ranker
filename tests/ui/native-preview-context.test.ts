import { beforeEach, describe, expect, it, vi } from "vitest";
const invoke = vi.hoisted(() => vi.fn());
const listen = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  isTauri: () => true,
  convertFileSrc: (path: string) => path,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
beforeEach(() => {
  vi.resetModules();
  invoke.mockReset();
  listen.mockReset();
});
describe("native-owned preview context", () => {
  it("binds a selected-image move to native context without renderer source paths", async () => {
    invoke.mockImplementation((command: string) =>
      Promise.resolve(command === "preview_context" ? "context-A" : {}),
    );
    const { api } = await import("../../src/native");
    const request = { imageIds: ["image-A", "image-B"], folderName: "Ink" };
    await api.moveSelectionToFolder(request);
    expect(invoke).toHaveBeenLastCalledWith("move_selection_to_folder", {
      contextId: "context-A",
      request,
    });
  });
  it("binds export dismissal to the exact operation and native context", async () => {
    invoke.mockImplementation((command: string) =>
      Promise.resolve(command === "preview_context" ? "context-A" : {}),
    );
    const { api } = await import("../../src/native");
    await api.recoverOperation("exfat-export-A", "abandon");
    expect(invoke).toHaveBeenLastCalledWith("recover_operation", {
      contextId: "context-A",
      id: "exfat-export-A",
      action: "abandon",
    });
  });
  it("binds restart-preservation commands to native context and omits an unhydrated expectation", async () => {
    invoke.mockImplementation((command: string) =>
      Promise.resolve(command === "preview_context" ? "context-A" : {}),
    );
    const { api } = await import("../../src/native");
    const expected = { schemaVersion: 1, name: "Exact pending renderer" };
    await api.prepareOperationRestart();
    expect(invoke).toHaveBeenLastCalledWith("prepare_operation_restart", {
      contextId: "context-A",
    });
    await api.prepareOperationRestart(expected);
    expect(invoke).toHaveBeenLastCalledWith("prepare_operation_restart", {
      contextId: "context-A",
      expectedCurrentSession: expected,
    });
    await api.exportOperationRestart("exact-token");
    expect(invoke).toHaveBeenLastCalledWith("export_operation_restart", {
      contextId: "context-A",
      token: "exact-token",
    });
    await api.confirmOperationRestart("exact-token", expected);
    expect(invoke).toHaveBeenLastCalledWith("confirm_operation_restart", {
      contextId: "context-A",
      token: "exact-token",
      expectedCurrentSession: expected,
    });
    await api.confirmOperationRestart("exact-token");
    expect(invoke).toHaveBeenLastCalledWith("confirm_operation_restart", {
      contextId: "context-A",
      token: "exact-token",
    });
    await api.cancelOperationRestart("exact-token");
    expect(invoke).toHaveBeenLastCalledWith("cancel_operation_restart", {
      contextId: "context-A",
      token: "exact-token",
    });
    expect(
      invoke.mock.calls.some(([name]) =>
        ["confirm_close", "retry_operation_publication", "save_ui"].includes(
          name,
        ),
      ),
    ).toBe(false);
  });
  it("announces only exact native file-publication restrictions and retries publication without repeating file work", async () => {
    let failure =
      "FILE_OPERATION_PUBLICATION_REQUIRED: retained settled worker";
    invoke.mockImplementation((command: string) =>
      command === "preview_context"
        ? Promise.resolve("context-A")
        : command === "retry_operation_publication"
          ? Promise.resolve(undefined)
          : Promise.reject(new Error(failure)),
    );
    const { api } = await import("../../src/native");
    const listener = vi.fn();
    api.onOperationRestriction?.(listener);
    await expect(api.commitOperation("operation-A")).rejects.toThrow(
      "FILE_OPERATION_PUBLICATION_REQUIRED",
    );
    expect(listener).toHaveBeenLastCalledWith({
      kind: "publication",
      message: failure,
    });
    await api.retryOperationPublication();
    expect(invoke).toHaveBeenCalledWith("retry_operation_publication", {
      contextId: "context-A",
    });
    expect(
      invoke.mock.calls.filter(([name]) => name === "commit_operation"),
    ).toHaveLength(1);
    failure = "FILE_OPERATION_RESTART_REQUIRED: unverified worker panic";
    await expect(api.undoOperation("operation-A")).rejects.toThrow(
      "FILE_OPERATION_RESTART_REQUIRED",
    );
    expect(listener).toHaveBeenLastCalledWith({
      kind: "restart",
      message: failure,
    });
    const calls = listener.mock.calls.length;
    failure = "IMAGE_WORK_BUSY: different operation";
    await expect(api.planCleanup("operation-A")).rejects.toThrow(
      "IMAGE_WORK_BUSY",
    );
    expect(listener).toHaveBeenCalledTimes(calls);
  });
  it("binds protection maintenance to current context while keeping approval token and durable request distinct", async () => {
    invoke.mockImplementation((command: string) =>
      Promise.resolve(command === "preview_context" ? "context-A" : {}),
    );
    const { api } = await import("../../src/native");
    const requestId = "a".repeat(64),
      token = "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa";
    await api.protectionMaintenanceStatus(requestId);
    await api.previewProtectionMaintenance();
    await api.confirmProtectionMaintenance(token);
    await api.resumeProtectionMaintenance(requestId);
    expect(invoke).toHaveBeenCalledWith("protection_maintenance_status", {
      contextId: "context-A",
      requestId,
    });
    expect(invoke).toHaveBeenCalledWith("preview_protection_maintenance", {
      contextId: "context-A",
    });
    expect(invoke).toHaveBeenCalledWith("confirm_protection_maintenance", {
      contextId: "context-A",
      token,
    });
    expect(invoke).toHaveBeenCalledWith("resume_protection_maintenance", {
      contextId: "context-A",
      requestId,
    });
  });
  it("binds profile expectation, candidate refresh and native export to the same renderer without repicking", async () => {
    invoke.mockImplementation((command: string) =>
      Promise.resolve(command === "preview_context" ? "context-A" : {}),
    );
    const { api } = await import("../../src/native");
    await api.chooseRecoveryStorage({ pending: "expected" });
    await api.openRecoveryProfile();
    await api.refreshProfileSwitch("candidate-A", { pending: "saved now" });
    await api.exportProfileReplacementCopy("candidate-B");
    expect(invoke).toHaveBeenCalledWith("choose_recovery_storage", {
      contextId: "context-A",
      expectedCurrentSession: { pending: "expected" },
    });
    expect(invoke).toHaveBeenCalledWith("open_recovery_profile", {
      contextId: "context-A",
      expectedCurrentSession: undefined,
    });
    expect(invoke).toHaveBeenCalledWith("refresh_profile_switch", {
      contextId: "context-A",
      token: "candidate-A",
      expectedCurrentSession: { pending: "saved now" },
    });
    expect(invoke).toHaveBeenCalledWith("export_profile_replacement_copy", {
      contextId: "context-A",
      token: "candidate-B",
    });
    expect(
      invoke.mock.calls.filter(([name]) => name === "preview_context"),
    ).toHaveLength(1);
  });
  it("retains the prior context and exact confirmation after uncertain profile selection, retiring only on known commit", async () => {
    let confirmed = false;
    const status = {
      mode: "recovery",
      state: "profile-selection-unresolved",
      contextId: "context-A",
      issues: [],
    };
    invoke.mockImplementation((command: string) => {
      if (command === "preview_context") return Promise.resolve("context-A");
      if (command === "get_boot_status") return Promise.resolve(status);
      if (command === "confirm_profile_switch")
        return confirmed
          ? Promise.resolve({ contextId: "context-B" })
          : Promise.reject(
              new Error("PROFILE_SELECTION_RECOVERY_REQUIRED: uncertain"),
            );
      return Promise.resolve({});
    });
    const { api } = await import("../../src/native");
    const listener = vi.fn();
    api.onActivationRestriction(listener);
    await expect(
      api.confirmProfileSwitch("same-token", "discard"),
    ).rejects.toThrow("PROFILE_SELECTION_RECOVERY_REQUIRED");
    expect(listener).toHaveBeenCalledWith(status);
    expect(await api.rendererContext()).toBe("context-A");
    confirmed = true;
    await api.confirmProfileSwitch("same-token", "discard");
    const calls = invoke.mock.calls.filter(
      ([name]) => name === "confirm_profile_switch",
    );
    expect(calls).toEqual(
      Array(2).fill([
        "confirm_profile_switch",
        { contextId: "context-A", token: "same-token", disposition: "discard" },
      ]),
    );
    await expect(api.saveSession({ old: "UI" })).rejects.toThrow(
      "STALE_CONTEXT",
    );
  });
  it("reports uncertain activation without retiring the context or preventing frozen export", async () => {
    const status = {
      mode: "recovery",
      state: "read-only-recovery",
      contextId: "context-A",
      issues: [
        { code: "activation-unresolved", message: "Commit needs recovery" },
      ],
    };
    invoke.mockImplementation((command: string) =>
      command === "preview_context"
        ? Promise.resolve("context-A")
        : command === "confirm_recovery_resume"
          ? Promise.reject(
              new Error("ACTIVATION_RECOVERY_REQUIRED: Commit needs recovery"),
            )
          : command === "get_boot_status"
            ? Promise.resolve(status)
            : Promise.resolve("copy.json"),
    );
    const { api } = await import("../../src/native");
    const listener = vi.fn();
    const off = api.onActivationRestriction(listener);
    await expect(
      api.confirmRecoveryResume("native-token", "replacement", "saved"),
    ).rejects.toThrow("ACTIVATION_RECOVERY_REQUIRED");
    expect(listener).toHaveBeenCalledExactlyOnceWith(status);
    await expect(api.exportSessionCopy({ pending: "retained" })).resolves.toBe(
      "copy.json",
    );
    expect(invoke).toHaveBeenCalledWith("export_session_copy", {
      contextId: "context-A",
      value: { pending: "retained" },
    });
    off();
    await expect(
      api.confirmRecoveryResume("native-token", "replacement", "saved"),
    ).rejects.toThrow("ACTIVATION_RECOVERY_REQUIRED");
    expect(listener).toHaveBeenCalledTimes(1);
  });
  it("ignores a recovery restriction for a different context and forwards exact request/preview identities", async () => {
    invoke.mockImplementation((command: string) =>
      command === "preview_context"
        ? Promise.resolve("context-A")
        : command === "confirm_recovery_resume"
          ? Promise.reject(new Error("ACTIVATION_RECOVERY_REQUIRED"))
          : command === "get_boot_status"
            ? Promise.resolve({ mode: "recovery", contextId: "context-B" })
            : Promise.resolve({}),
    );
    const { api } = await import("../../src/native");
    const listener = vi.fn();
    api.onActivationRestriction(listener);
    await api.recoveryStorageStatus();
    await api.prepareRecoveryResume("durable-request");
    await api.cancelRecoveryResume("ephemeral-token");
    await api.previewRecoveryCleanup("durable-request");
    await api.cancelRecoveryCleanup("cleanup-token");
    await api.confirmRecoveryCleanup("cleanup-token");
    await expect(
      api.confirmRecoveryResume("ephemeral-token", "replacement", "saved"),
    ).rejects.toThrow("ACTIVATION_RECOVERY_REQUIRED");
    expect(listener).not.toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledWith("prepare_recovery_resume", {
      contextId: "context-A",
      requestId: "durable-request",
    });
    expect(invoke).toHaveBeenCalledWith("confirm_recovery_cleanup", {
      contextId: "context-A",
      token: "cleanup-token",
    });
  });
  it("retires the old context only after a retained recovery request commits", async () => {
    invoke.mockImplementation((command: string) =>
      Promise.resolve(
        command === "preview_context"
          ? "context-A"
          : { contextId: "context-B" },
      ),
    );
    const { api } = await import("../../src/native");
    await api.confirmRecoveryResume("resume-token", "replacement", "saved");
    await expect(api.saveSession({ pending: "old" })).rejects.toThrow(
      "STALE_CONTEXT",
    );
    expect(invoke.mock.calls.some((call) => call[0] === "save_session")).toBe(
      false,
    );
  });
  it("binds worker status, targeted cancellation and cleanup to the renderer context", async () => {
    invoke.mockImplementation((command: string) =>
      Promise.resolve(command === "preview_context" ? "context-A" : {}),
    );
    const { api } = await import("../../src/native");
    await api.imageWorkStatus();
    await api.cancelImageWork("preview-request");
    await api.cancelImageWork();
    await api.reapImageWorker();
    expect(invoke).toHaveBeenCalledWith("image_work_status", {
      contextId: "context-A",
    });
    expect(invoke).toHaveBeenCalledWith("cancel_image_work", {
      contextId: "context-A",
      requestId: "preview-request",
    });
    expect(invoke).toHaveBeenCalledWith("cancel_image_work", {
      contextId: "context-A",
      requestId: null,
    });
    expect(invoke).toHaveBeenCalledWith("reap_image_worker", {
      contextId: "context-A",
    });
    expect(
      invoke.mock.calls.filter((call) => call[0] === "preview_context"),
    ).toHaveLength(1);
  });
  it("shares one lazily acquired context across concurrent four-argument preview calls", async () => {
    const context = "55311f68-d1ad-4c48-a012-e8ebf9322c38";
    let finish!: (value: string) => void;
    invoke.mockImplementation((command: string, args: unknown) =>
      command === "preview_context"
        ? new Promise((resolve) => {
            finish = resolve;
          })
        : Promise.resolve(args),
    );
    const { api } = await import("../../src/native");
    const first = api.requestPreview("a", 512, "rev-a", "req-a"),
      second = api.requestPreview("b", 2048, "rev-b", "req-b");
    expect(invoke.mock.calls.map((call) => call[0])).toEqual([
      "preview_context",
    ]);
    finish(context);
    await Promise.all([first, second]);
    expect(invoke).toHaveBeenCalledWith("request_preview", {
      id: "a",
      size: 512,
      expectedContentRevision: "rev-a",
      requestId: "req-a",
      contextId: context,
    });
    expect(invoke).toHaveBeenCalledWith("request_preview", {
      id: "b",
      size: 2048,
      expectedContentRevision: "rev-b",
      requestId: "req-b",
      contextId: context,
    });
    await api.requestPreview("c", 512, "rev-c", "req-c");
    expect(
      invoke.mock.calls.filter((call) => call[0] === "preview_context"),
    ).toHaveLength(1);
  });
  it("retries failed context acquisition and forwards releases without inventing another context", async () => {
    let attempts = 0;
    invoke.mockImplementation((command: string) =>
      command === "preview_context"
        ? ++attempts === 1
          ? Promise.reject(new Error("not ready"))
          : Promise.resolve("native-context")
        : Promise.resolve({}),
    );
    const { api } = await import("../../src/native");
    await expect(api.requestPreview("a", 512, "rev", "req")).rejects.toThrow(
      "not ready",
    );
    await api.requestPreview("a", 512, "rev", "req2");
    expect(attempts).toBe(2);
    await api.releasePreview("held-lease");
    expect(invoke).toHaveBeenCalledWith("release_preview", {
      leaseId: "held-lease",
      contextId: "native-context",
    });
    expect(attempts).toBe(2);
  });
  it("binds stateful calls to the original context and never reacquires on a stale-context rejection", async () => {
    invoke.mockImplementation((command: string) =>
      command === "preview_context"
        ? Promise.resolve("context-A")
        : Promise.reject(new Error("STALE_CONTEXT")),
    );
    const { api } = await import("../../src/native");
    await expect(api.saveSession({ name: "pending" })).rejects.toThrow(
      "STALE_CONTEXT",
    );
    await expect(api.chooseDestination()).rejects.toThrow("STALE_CONTEXT");
    expect(
      invoke.mock.calls.filter((call) => call[0] === "preview_context"),
    ).toHaveLength(1);
    expect(invoke).toHaveBeenCalledWith("save_session", {
      value: { name: "pending" },
      contextId: "context-A",
    });
    expect(invoke).toHaveBeenCalledWith("choose_destination", {
      contextId: "context-A",
    });
  });
  it("retires queued responses and refuses new old-profile writes immediately after native confirmation", async () => {
    let resolvePlan!: (value: unknown) => void;
    invoke.mockImplementation((command: string) =>
      command === "preview_context"
        ? Promise.resolve("context-A")
        : command === "plan_operation"
          ? new Promise((resolve) => {
              resolvePlan = resolve;
            })
          : Promise.resolve({ contextId: "context-B", profileGeneration: "B" }),
    );
    const { api } = await import("../../src/native");
    const oldPlan = api.planOperation({ kind: "copy" } as never);
    await Promise.resolve();
    await Promise.resolve();
    await api.confirmProfileSwitch("prepared-token", "recovery-copy");
    resolvePlan({ id: "old-plan" });
    await expect(oldPlan).rejects.toThrow("STALE_CONTEXT");
    await expect(api.saveSession({ name: "must-not-write" })).rejects.toThrow(
      "STALE_CONTEXT",
    );
    expect(invoke.mock.calls.some((call) => call[0] === "save_session")).toBe(
      false,
    );
    expect(invoke).toHaveBeenCalledWith("confirm_profile_switch", {
      token: "prepared-token",
      disposition: "recovery-copy",
      contextId: "context-A",
    });
  });
  it("ignores progress and close events from a different or retired renderer context", async () => {
    const handlers = new Map<string, (event: { payload: unknown }) => void>();
    listen.mockImplementation(
      async (name: string, handler: (event: { payload: unknown }) => void) => {
        handlers.set(name, handler);
        return () => {};
      },
    );
    invoke.mockImplementation((command: string) =>
      Promise.resolve(
        command === "preview_context"
          ? "context-A"
          : { contextId: "context-B" },
      ),
    );
    const { api } = await import("../../src/native");
    const scan = vi.fn(),
      operation = vi.fn(),
      close = vi.fn();
    await api.subscribeScanProgress(scan);
    await api.subscribeOperationProgress(operation);
    await api.subscribeCloseRequested(close);
    for (const [name, handler] of handlers) {
      handler({ payload: { contextId: "wrong", token: "wrong" } });
      handler({ payload: {} });
      handler({ payload: { contextId: "context-A", token: name } });
    }
    expect(scan).toHaveBeenCalledTimes(1);
    expect(operation).toHaveBeenCalledTimes(1);
    expect(close).toHaveBeenCalledTimes(1);
    await api.confirmProfileSwitch("token", "saved");
    for (const handler of handlers.values())
      handler({ payload: { contextId: "context-A" } });
    expect(scan).toHaveBeenCalledTimes(1);
    expect(operation).toHaveBeenCalledTimes(1);
    expect(close).toHaveBeenCalledTimes(1);
  });
  it("retires the old session bridge after a recovery-copy restore as well as a profile switch", async () => {
    invoke.mockImplementation((command: string) =>
      Promise.resolve(
        command === "preview_context"
          ? "old-session-context"
          : { contextId: "restored-session-context" },
      ),
    );
    const { api } = await import("../../src/native");
    await api.restoreRecoveryCopy(
      "inspected-copy",
      true,
      "replacement",
      "recovery-copy",
    );
    expect(invoke).toHaveBeenCalledWith("restore_recovery_copy", {
      token: "inspected-copy",
      acceptPending: true,
      replacementToken: "replacement",
      disposition: "recovery-copy",
      contextId: "old-session-context",
    });
    await expect(api.saveSession({ name: "old-session" })).rejects.toThrow(
      "STALE_CONTEXT",
    );
    expect(invoke.mock.calls.some((call) => call[0] === "save_session")).toBe(
      false,
    );
  });
});

it("forwards native replacement subject, optional actual renderer state, and token-only export", async () => {
  invoke.mockImplementation((command: string) =>
    Promise.resolve(command === "preview_context" ? "context-A" : {}),
  );
  const { api } = await import("../../src/native");
  const subject = {
    kind: "fresh" as const,
    token: "imported",
    acceptPending: true,
  };
  await api.prepareRecoveryReplacement(subject, {
    name: "pending current state",
  });
  await api.prepareRecoveryReplacement({ kind: "resume", token: "retained" });
  await api.exportRecoveryReplacementCopy("replacement-token");
  await api.cancelRecoveryReplacement("replacement-token");
  expect(invoke).toHaveBeenCalledWith("prepare_recovery_replacement", {
    contextId: "context-A",
    subject,
    expectedCurrentSession: { name: "pending current state" },
  });
  expect(invoke).toHaveBeenCalledWith("prepare_recovery_replacement", {
    contextId: "context-A",
    subject: { kind: "resume", token: "retained" },
    expectedCurrentSession: undefined,
  });
  expect(invoke).toHaveBeenCalledWith("export_recovery_replacement_copy", {
    contextId: "context-A",
    token: "replacement-token",
  });
  expect(invoke).toHaveBeenCalledWith("cancel_recovery_replacement", {
    contextId: "context-A",
    token: "replacement-token",
  });
});

it("does not publish a late old-context file restriction after native profile replacement", async () => {
  let reject!: (reason: unknown) => void;
  invoke.mockImplementation((name: string) =>
    name === "preview_context"
      ? Promise.resolve("old-context")
      : name === "commit_operation"
        ? new Promise((_done, fail) => {
            reject = fail;
          })
        : Promise.resolve({ contextId: "new-context" }),
  );
  const { api } = await import("../../src/native");
  const listener = vi.fn();
  api.onOperationRestriction(listener);
  const command = api.commitOperation("old-operation");
  const rejected = expect(command).rejects.toThrow(
    "FILE_OPERATION_PUBLICATION_REQUIRED",
  );
  await vi.waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("commit_operation", {
      id: "old-operation",
      contextId: "old-context",
    }),
  );
  await api.confirmProfileSwitch("selection", "saved");
  reject(new Error("FILE_OPERATION_PUBLICATION_REQUIRED: old result"));
  await rejected;
  expect(listener).not.toHaveBeenCalled();
});
