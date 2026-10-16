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
import { ProtectionMaintenance } from "../../src/components/ProtectionMaintenance";
import type {
  ProtectionMaintenancePreview,
  ProtectionMaintenanceResult,
  ProtectionMaintenanceStatus,
  RecoveryWorkJob,
} from "../../src/native";
vi.mock("../../src/native", () => ({ api: {} }));
afterEach(() => {
  cleanup();
  localStorage.clear();
});
const requestId = "a".repeat(64),
  otherId = "b".repeat(64);
const result: ProtectionMaintenanceResult = {
  requestId,
  state: "completed",
  generation: "2",
  beforeBytes: 2000,
  afterBytes: 1000,
  preservedClaims: 50,
  reservedCompletionBytes: 700,
  reservedCompletionClaims: 2,
};
const preview: ProtectionMaintenancePreview = {
  token: "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa",
  requestId,
  contextId: "native-context",
  profileGeneration: "native-profile",
  expiresInMs: 300000,
  plan: {
    token: requestId,
    generation: "1",
    usedBytes: 2000,
    compactedBytesUpperBound: 1200,
    reclaimableBytesLowerBound: 800,
    preservedClaims: 50,
    addedManagedClaims: 2,
    reservedCompletionBytes: 700,
    reservedCompletionClaims: 2,
    extraDiskBytesUpperBound: 4500,
    canReclaim: true,
  },
};
const status: ProtectionMaintenanceStatus = {
  registry: {
    requestId: null,
    state: "ready",
    generation: "1",
    canResume: false,
    lastResult: null,
  },
  result: null,
  active: false,
  fresh: true,
  error: null,
};
const idle = { active: null, recent: [], limit: 1 };
const worker: RecoveryWorkJob = {
  id: "registry-worker",
  contextId: "prior-renderer",
  profileGeneration: "native-profile",
  kind: "registry-maintenance",
  phase: "validating-maintenance",
  state: "running",
  completed: 0,
  total: null,
  cancelRequested: false,
  message: null,
};
const deferred = <T,>() => {
  let resolve!: (value: T) => void, reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
};
function setup(open = true, initialWorker: RecoveryWorkJob | null = null) {
  const actions = {
    status: vi.fn().mockResolvedValue(status),
    preview: vi.fn().mockResolvedValue(preview),
    confirm: vi.fn().mockResolvedValue(result),
    resume: vi.fn().mockResolvedValue(result),
    context: vi.fn().mockResolvedValue("native-context"),
  };
  const work = {
    status: vi.fn().mockResolvedValue({ ...idle, active: initialWorker }),
    cancel: vi.fn().mockResolvedValue({ id: worker.id, cancelRequested: true }),
  };
  const props = {
    open,
    actions,
    work,
    profileGeneration: "native-profile",
    onOpen: vi.fn(),
    onClose: vi.fn(),
    onHelp: vi.fn(),
    onBusyChange: vi.fn(),
    flush: vi.fn().mockResolvedValue(undefined),
  };
  const view = render(<ProtectionMaintenance {...props} />);
  return { ...view, props, actions, work };
}
async function click(name: string) {
  const button = await screen.findByRole("button", { name });
  await waitFor(() => expect(button.hasAttribute("disabled")).toBe(false));
  fireEvent.click(button);
}
async function prepare() {
  await click("Review protection maintenance");
  await screen.findByRole("region", { name: "Protection maintenance preview" });
}
async function approve() {
  const checkbox = screen.getByRole("checkbox", {
    name: /I reviewed the preserved protections/,
  });
  await waitFor(() => expect(checkbox.hasAttribute("disabled")).toBe(false));
  fireEvent.click(checkbox);
  await click("Approve protection maintenance");
}
async function refreshWork() {
  await click("Refresh recovery work status");
  await act(async () => {});
}
it("requires explicit review and sends only the native UUID, preserving bounds and reserves", async () => {
  const s = setup();
  await prepare();
  expect(s.actions.confirm).not.toHaveBeenCalled();
  expect(screen.getByText("4,500 bytes")).toBeTruthy();
  expect(screen.getByText("700 bytes · 2 claims")).toBeTruthy();
  expect(
    screen
      .getByRole("button", { name: "Approve protection maintenance" })
      .hasAttribute("disabled"),
  ).toBe(true);
  await approve();
  await screen.findByRole("heading", {
    name: "Protection maintenance completed",
  });
  expect(s.actions.confirm).toHaveBeenCalledExactlyOnceWith(preview.token);
  expect(s.actions.resume).not.toHaveBeenCalled();
});
it.each([
  "STALE_REGISTRY_PREVIEW",
  "PROTECTED_TARGET_UNRESOLVED: maintenance preview is stale; approve a new preview",
])("requires a fresh approval after %s", async (code) => {
  const s = setup();
  s.actions.confirm.mockRejectedValueOnce(new Error(code));
  await prepare();
  await approve();
  await screen.findByRole("alert");
  expect(screen.queryByRole("checkbox", { name: /I reviewed/ })).toBeNull();
  await prepare();
  expect(
    (screen.getByRole("checkbox", { name: /I reviewed/ }) as HTMLInputElement)
      .checked,
  ).toBe(false);
  expect(s.actions.confirm).toHaveBeenCalledTimes(1);
});
it("rejects a preview from a different native renderer or profile", async () => {
  const s = setup();
  s.actions.preview.mockResolvedValue({ ...preview, contextId: "other" });
  await click("Review protection maintenance");
  expect((await screen.findByRole("alert")).textContent).toContain(
    "STALE_REGISTRY_PREVIEW",
  );
  expect(screen.queryByRole("checkbox", { name: /I reviewed/ })).toBeNull();
  expect(s.actions.confirm).not.toHaveBeenCalled();
});
it("shows no-reclaim bounds without offering removal of unique historical protections", async () => {
  const s = setup();
  s.actions.preview.mockResolvedValue({
    ...preview,
    plan: { ...preview.plan, canReclaim: false, reclaimableBytesLowerBound: 0 },
  });
  await prepare();
  expect(
    screen.getByText(
      /Unique historical protections and reserved completions cannot be removed/,
    ),
  ).toBeTruthy();
  expect(
    screen.queryByRole("button", { name: "Approve protection maintenance" }),
  ).toBeNull();
  expect(s.actions.confirm).not.toHaveBeenCalled();
});
it("reports an exact native no-reclaim receipt without claiming bytes were reclaimed", async () => {
  const s = setup();
  s.actions.confirm.mockResolvedValue({
    ...result,
    state: "no-reclaim",
    afterBytes: result.beforeBytes,
  });
  await prepare();
  await approve();
  await screen.findByRole("heading", {
    name: "Protection maintenance found no reclaimable space",
  });
  expect(
    screen.queryByRole("heading", { name: "Protection maintenance completed" }),
  ).toBeNull();
  expect(
    screen.getByText(/2,000 bytes before · 2,000 bytes after/),
  ).toBeTruthy();
});
it("retains a lost-response ID across remount and resolves only its exact native result", async () => {
  const s = setup();
  s.actions.confirm.mockRejectedValueOnce(new Error("response lost"));
  await prepare();
  await approve();
  await screen.findByRole("alert");
  s.unmount();
  const next = setup();
  next.actions.status.mockResolvedValue({ ...status, result });
  await click("Check retained maintenance request");
  await screen.findByRole("heading", {
    name: "Protection maintenance completed",
  });
  expect(next.actions.status).toHaveBeenCalledExactlyOnceWith(requestId);
  expect(next.actions.confirm).not.toHaveBeenCalled();
});
it("never adopts another result or a nested latest result for an unknown retained ID", async () => {
  localStorage.setItem(
    "image-ranker-protection-maintenance-request",
    requestId,
  );
  const s = setup();
  s.actions.status.mockResolvedValue({
    ...status,
    registry: {
      ...status.registry,
      lastResult: { ...result, requestId: otherId },
    },
  });
  await click("Check retained maintenance request");
  await screen.findByText(/No exact outcome is available/);
  expect(
    screen.queryByRole("region", { name: "Exact maintenance result" }),
  ).toBeNull();
  s.actions.status.mockResolvedValue({
    ...status,
    result: { ...result, requestId: otherId },
  });
  await click("Check retained maintenance request");
  expect((await screen.findByRole("alert")).textContent).toContain(
    "another request",
  );
  expect(
    screen.queryByRole("region", { name: "Exact maintenance result" }),
  ).toBeNull();
});
it("rejects a mismatched direct completion and retains the original request", async () => {
  const s = setup();
  s.actions.confirm.mockResolvedValue({ ...result, requestId: otherId });
  await prepare();
  await approve();
  expect((await screen.findByRole("alert")).textContent).toContain(
    "different or incomplete",
  );
  expect(
    localStorage.getItem("image-ranker-protection-maintenance-request"),
  ).toBe(requestId);
  expect(
    screen.queryByRole("region", { name: "Exact maintenance result" }),
  ).toBeNull();
});
it("resumes only an exact freshly observed retained request", async () => {
  localStorage.setItem(
    "image-ranker-protection-maintenance-request",
    requestId,
  );
  const s = setup();
  s.actions.status.mockResolvedValue({
    ...status,
    registry: {
      ...status.registry,
      requestId,
      state: "cleanup-pending",
      canResume: true,
    },
  });
  await click("Check retained maintenance request");
  await click("Resume retained maintenance request");
  await screen.findByRole("heading", {
    name: "Protection maintenance completed",
  });
  expect(s.actions.resume).toHaveBeenCalledExactlyOnceWith(requestId);
  expect(s.actions.confirm).not.toHaveBeenCalled();
});
it("cached or failed status cannot enable resume or erase an unconfirmed outcome", async () => {
  localStorage.setItem(
    "image-ranker-protection-maintenance-request",
    requestId,
  );
  const s = setup();
  s.actions.status.mockResolvedValue({
    ...status,
    active: true,
    fresh: false,
    registry: { ...status.registry, requestId, canResume: true },
  });
  await click("Check retained maintenance request");
  await screen.findByText(/cached observation/);
  expect(
    screen.queryByRole("button", {
      name: "Resume retained maintenance request",
    }),
  ).toBeNull();
  s.actions.status.mockRejectedValue(new Error("SUPPORT_BUSY"));
  await click("Check retained maintenance request");
  expect((await screen.findByRole("alert")).textContent).toContain(
    "SUPPORT_BUSY",
  );
  expect(screen.queryByText(/Native storage state/)).toBeNull();
});
it.each(["command-first", "worker-first"])(
  "keeps editing frozen after dialog dismissal until command and worker finish (%s)",
  async (order) => {
    const s = setup();
    const pending = deferred<ProtectionMaintenanceResult>();
    s.actions.confirm.mockReturnValue(pending.promise);
    await prepare();
    await approve();
    s.work.status.mockResolvedValue({ ...idle, active: worker });
    await refreshWork();
    s.rerender(<ProtectionMaintenance {...s.props} open={false} />);
    await act(async () => {});
    expect(s.props.onBusyChange).toHaveBeenLastCalledWith(true);
    if (order === "command-first") {
      await act(async () => pending.resolve(result));
      s.work.status.mockResolvedValue(idle);
      await refreshWork();
    } else {
      s.work.status.mockResolvedValue({
        ...idle,
        recent: [{ ...worker, state: "completed" }],
      });
      await refreshWork();
    }
    expect(s.props.onBusyChange).toHaveBeenLastCalledWith(true);
    s.work.status.mockResolvedValue({
      ...idle,
      recent: [{ ...worker, state: "completed" }],
    });
    if (order === "worker-first")
      await act(async () => pending.resolve(result));
    else await refreshWork();
    await waitFor(() =>
      expect(s.props.onBusyChange).toHaveBeenLastCalledWith(false),
    );
  },
);
it("allows Help and exact worker cancellation without waiting for maintenance command return", async () => {
  const s = setup();
  const pending = deferred<ProtectionMaintenanceResult>();
  s.actions.confirm.mockReturnValue(pending.promise);
  await prepare();
  await approve();
  s.work.status.mockResolvedValue({ ...idle, active: worker });
  await refreshWork();
  await click("Help and diagnostics");
  expect(s.props.onHelp).toHaveBeenCalledOnce();
  await click("Cancel recovery work");
  await screen.findByText(/^Cancellation requested\. Recovery work/);
  expect(s.work.cancel).toHaveBeenCalledWith(worker.id);
  s.work.status.mockResolvedValue({
    ...idle,
    recent: [{ ...worker, state: "cancelled" }],
  });
  await refreshWork();
  expect(s.props.onBusyChange).toHaveBeenLastCalledWith(true);
  await act(async () =>
    pending.reject(new Error("RECOVERY_CANCELLED: stopped before mutation")),
  );
  await screen.findByRole("alert");
  expect(
    screen.queryByRole("region", { name: "Exact maintenance result" }),
  ).toBeNull();
});
it("keeps sealed maintenance finishing until its native terminal outcome", async () => {
  const s = setup(false, { ...worker, phase: "finishing-maintenance" });
  await act(async () => {});
  expect(
    screen
      .getByRole("button", { name: "Cancel recovery work" })
      .hasAttribute("disabled"),
  ).toBe(true);
  expect(screen.getByText(/Protection maintenance is finishing/)).toBeTruthy();
  s.work.status.mockResolvedValue(idle);
  await refreshWork();
  expect(s.props.onBusyChange).toHaveBeenLastCalledWith(true);
});
