// @vitest-environment jsdom
import { StrictMode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import {
  ProfileSwitcher,
  type ProfileSwitchPreview,
} from "../../src/components/ProfileSwitcher";

afterEach(cleanup);
const prepared: ProfileSwitchPreview = {
  token: "picker-granted-switch",
  mode: "open",
  profile: {
    id: "profile-B",
    displayPath: "D:/Recovery/Image Ranker",
    isDefault: false,
    writable: true,
  },
  hasPendingSave: false,
  pendingOperations: 0,
  requiresDisposition: false,
  rendererChangesUnadmitted: false,
  warnings: ["Source grants remain scoped to this profile."],
};
const boot = {
  mode: "ready" as const,
  issues: [],
  canRestoreBackup: true,
  profile: prepared.profile,
  profileGeneration: "native-profile-B-generation",
};
const pendingSnapshot = {
  schemaVersion: 1,
  run: { history: { chunks: [[{ choice: "left" }]], length: 1 } },
  archivedRuns: [{ history: "retained" }],
  review: { picks: ["a"] },
};
const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((accept) => {
    resolve = accept;
  });
  return { promise, resolve };
};
function setup(
  overrides: Partial<
    Omit<Parameters<typeof ProfileSwitcher>[0], "commands">
  > = {},
  strict = false,
) {
  const props = {
    mode: "open" as const,
    current: {
      id: "profile-A",
      displayPath: "C:/Unavailable/Profile",
      isDefault: true,
      writable: false,
    },
    generation: 1,
    commands: {
      prepare: vi.fn().mockResolvedValue(structuredClone(prepared)),
      refresh: vi
        .fn()
        .mockResolvedValue({ ...prepared, token: "renewed-candidate" }),
      confirm: vi.fn().mockResolvedValue(boot),
      cancel: vi.fn().mockResolvedValue(undefined),
      exportCopy: vi.fn().mockResolvedValue("E:/Recovery/pending.json"),
    },
    flush: vi.fn().mockResolvedValue(undefined),
    retry: vi.fn().mockResolvedValue(undefined),
    snapshot: vi.fn(() => pendingSnapshot),
    onClose: vi.fn(),
    onSwitched: vi.fn(),
    ...overrides,
  };
  const view = render(
    strict ? (
      <StrictMode>
        <ProfileSwitcher {...props} />
      </StrictMode>
    ) : (
      <ProfileSwitcher {...props} />
    ),
  );
  return { props, ...view };
}
async function approve() {
  const input = await screen.findByRole("checkbox", {
    name: "I reviewed this profile and its location.",
  });
  fireEvent.click(input);
}

describe("picker-granted recovery profile switch", () => {
  it("waits for the durable queue before preparing, and requires review of the exact destination", async () => {
    const queued = deferred<void>();
    const { props } = setup({ flush: () => queued.promise });
    expect(props.commands.prepare).not.toHaveBeenCalled();
    expect(
      screen
        .getByRole("button", { name: "Keep current profile" })
        .hasAttribute("disabled"),
    ).toBe(true);
    await act(async () => queued.resolve());
    await screen.findByText(prepared.profile.displayPath);
    expect(props.commands.prepare).toHaveBeenCalledWith(
      "open",
      pendingSnapshot,
    );
    expect(
      screen
        .getByRole("button", { name: "Switch profile" })
        .hasAttribute("disabled"),
    ).toBe(true);
    await approve();
    fireEvent.click(screen.getByRole("button", { name: "Switch profile" }));
    await waitFor(() =>
      expect(props.commands.confirm).toHaveBeenCalledWith(
        prepared.token,
        "saved",
      ),
    );
    expect(props.onSwitched).toHaveBeenCalledWith(boot);
  });
  it("allows preparing a new location even when the current storage is unavailable", async () => {
    const { props } = setup({ mode: "create" });
    await screen.findByText(prepared.profile.displayPath);
    expect(props.commands.prepare).toHaveBeenCalledWith(
      "create",
      pendingSnapshot,
    );
    expect(screen.getByText("C:/Unavailable/Profile")).toBeTruthy();
    expect(props.commands.confirm).not.toHaveBeenCalled();
  });
  it("cancels the picker without committing or inventing an empty saved session", async () => {
    const { props } = setup();
    props.commands.prepare.mockResolvedValue(null);
    await waitFor(() => expect(props.onClose).toHaveBeenCalledTimes(1));
    expect(props.commands.confirm).not.toHaveBeenCalled();
    expect(props.onSwitched).not.toHaveBeenCalled();
    expect(props.snapshot).toHaveBeenCalledTimes(1);
  });
  it("keeps the current profile and pending work after preparation failure", async () => {
    const { props } = setup();
    props.commands.prepare.mockRejectedValue(
      new Error("PROFILE_UNAVAILABLE: permission denied"),
    );
    await screen.findByText(/PROFILE_UNAVAILABLE/);
    expect(
      screen.getByText(
        /Your current profile and pending work remain unchanged/,
      ),
    ).toBeTruthy();
    fireEvent.click(
      screen.getByRole("button", { name: "Keep current profile" }),
    );
    await waitFor(() => expect(props.onClose).toHaveBeenCalled());
    expect(props.commands.confirm).not.toHaveBeenCalled();
    expect(props.onSwitched).not.toHaveBeenCalled();
  });
  it("does not treat failed queue drain or cancelled export as a saved boundary", async () => {
    const { props } = setup({
      flush: vi.fn().mockRejectedValue(new Error("Storage full")),
    });
    props.commands.exportCopy.mockResolvedValue(null);
    await approve();
    expect(screen.queryByRole("button", { name: "Switch profile" })).toBeNull();
    fireEvent.click(
      screen.getByRole("button", { name: "Save recovery copy and switch" }),
    );
    await screen.findByText(/Recovery copy cancelled/);
    expect(props.commands.exportCopy).toHaveBeenCalledWith(prepared.token);
    expect(props.commands.exportCopy.mock.calls[0][0]).toBe(prepared.token);
    expect(props.commands.confirm).not.toHaveBeenCalled();
    expect(props.onClose).not.toHaveBeenCalled();
    expect(props.onSwitched).not.toHaveBeenCalled();
  });
  it("preserves pending history when export fails, then uses native receipt verification on success", async () => {
    const { props } = setup({
      flush: vi.fn().mockRejectedValue(new Error("Storage full")),
    });
    props.commands.exportCopy.mockRejectedValueOnce(
      new Error("Export write failed"),
    );
    await approve();
    fireEvent.click(
      screen.getByRole("button", { name: "Save recovery copy and switch" }),
    );
    await screen.findByText("Error: Export write failed");
    expect(props.commands.confirm).not.toHaveBeenCalled();
    fireEvent.click(
      screen.getByRole("button", { name: "Save recovery copy and switch" }),
    );
    await waitFor(() =>
      expect(props.commands.confirm).toHaveBeenCalledWith(
        prepared.token,
        "recovery-copy",
      ),
    );
    expect(props.commands.exportCopy.mock.calls[1][0]).toEqual(prepared.token);
    expect(props.onSwitched).toHaveBeenCalledWith(boot);
  });
  it("does not switch when native rejects a stale recovery-copy receipt", async () => {
    const { props } = setup({
      flush: vi.fn().mockRejectedValue(new Error("Save failed")),
    });
    props.commands.confirm.mockRejectedValue(
      new Error("PROFILE_COPY_REQUIRED"),
    );
    await approve();
    fireEvent.click(
      screen.getByRole("button", { name: "Save recovery copy and switch" }),
    );
    await screen.findByText("Error: PROFILE_COPY_REQUIRED");
    expect(props.onSwitched).not.toHaveBeenCalled();
    expect(props.onClose).not.toHaveBeenCalled();
    expect(
      screen
        .getByRole("button", { name: "Keep current profile" })
        .hasAttribute("disabled"),
    ).toBe(false);
  });
  it("retries pending saves and drains again before confirming a saved switch", async () => {
    const flush = vi
      .fn()
      .mockRejectedValueOnce(new Error("Save failed"))
      .mockResolvedValue(undefined);
    const retry = deferred<void>();
    const { props } = setup({ flush, retry: () => retry.promise });
    await approve();
    fireEvent.click(
      screen.getByRole("button", { name: "Retry save and review profile" }),
    );
    expect(props.commands.confirm).not.toHaveBeenCalled();
    await act(async () => retry.resolve());
    expect(props.commands.refresh).toHaveBeenCalledWith(
      prepared.token,
      pendingSnapshot,
    );
    expect(props.commands.confirm).not.toHaveBeenCalled();
    await approve();
    fireEvent.click(screen.getByRole("button", { name: "Switch profile" }));
    await waitFor(() =>
      expect(props.commands.confirm).toHaveBeenCalledWith(
        "renewed-candidate",
        "saved",
      ),
    );
    expect(flush).toHaveBeenCalledTimes(2);
  });
  it("requires both profile review and explicit discard before passing discard to native", async () => {
    const { props } = setup({
      flush: vi.fn().mockRejectedValue(new Error("Save failed")),
    });
    await approve();
    const button = screen.getByRole("button", {
      name: "Switch and discard pending edits",
    });
    expect(button.hasAttribute("disabled")).toBe(true);
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: /Discard only the current unsaved edits/,
      }),
    );
    fireEvent.click(button);
    await waitFor(() =>
      expect(props.commands.confirm).toHaveBeenCalledWith(
        prepared.token,
        "discard",
      ),
    );
    expect(props.commands.exportCopy).not.toHaveBeenCalled();
  });
  it("cancels a prepared token before unfreezing the existing profile", async () => {
    const { props } = setup();
    await approve();
    const cancelled = deferred<void>();
    props.commands.cancel.mockImplementation(() => cancelled.promise);
    fireEvent.click(
      screen.getByRole("button", { name: "Keep current profile" }),
    );
    expect(props.commands.cancel).toHaveBeenCalledWith(prepared.token);
    expect(props.onClose).not.toHaveBeenCalled();
    await act(async () => cancelled.resolve());
    expect(props.onClose).toHaveBeenCalled();
    expect(props.commands.confirm).not.toHaveBeenCalled();
  });
  it("cancels a late picker result after the view has unmounted", async () => {
    const chosen = deferred<ProfileSwitchPreview>();
    const { props, unmount } = setup();
    props.commands.prepare.mockImplementation(() => chosen.promise);
    await waitFor(() => expect(props.commands.prepare).toHaveBeenCalled());
    unmount();
    await act(async () => chosen.resolve(prepared));
    expect(props.commands.cancel).toHaveBeenCalledWith(prepared.token);
    expect(props.onSwitched).not.toHaveBeenCalled();
  });
  it("ignores and retires a response from a stale profile generation", async () => {
    const chosen = deferred<ProfileSwitchPreview>();
    const { props, rerender } = setup();
    props.commands.prepare.mockImplementation(() => chosen.promise);
    await waitFor(() => expect(props.commands.prepare).toHaveBeenCalled());
    rerender(<ProfileSwitcher {...props} generation={2} />);
    await act(async () => chosen.resolve(prepared));
    expect(props.commands.cancel).toHaveBeenCalledWith(prepared.token);
    expect(props.commands.confirm).not.toHaveBeenCalled();
    expect(screen.queryByText(prepared.profile.displayPath)).toBeNull();
  });
  it("opens only one native picker under StrictMode effect replay", async () => {
    const { props } = setup({}, true);
    await approve();
    expect(props.commands.prepare).toHaveBeenCalledTimes(1);
  });
  it("keeps the old workspace frozen if reload fails after native commit", async () => {
    const { props } = setup({
      onSwitched: vi.fn().mockRejectedValue(new Error("Navigation blocked")),
    });
    await approve();
    fireEvent.click(screen.getByRole("button", { name: "Switch profile" }));
    await screen.findByText(
      /The profile was switched, but its view could not reload/,
    );
    expect(props.commands.confirm).toHaveBeenCalledTimes(1);
    expect(
      screen
        .getByRole("button", { name: "Keep current profile" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(props.onClose).not.toHaveBeenCalled();
  });
});
