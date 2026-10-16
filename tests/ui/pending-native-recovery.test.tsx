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
import { StartupRecovery } from "../../src/components/StartupRecovery";
vi.mock("../../src/native", () => ({ desktop: true, api: {} }));
afterEach(cleanup);
const pending = {
  token: "native-persisted-selection",
  disposition: "recovery-copy" as const,
  profile: {
    id: "selected-profile",
    displayPath: "D:/Recorded/Profile",
    isDefault: false,
    writable: true,
  },
  warnings: ["Native recovery will revalidate the recorded authorization."],
};
const boot = {
  mode: "recovery" as const,
  state: "profile-selection-unresolved",
  issues: [],
  canRestoreBackup: false,
  canChooseRecoveryStorage: false,
  pendingProfileSwitch: pending,
};
const ready = { ...boot, mode: "ready" as const, state: "ready" };
const idle = { active: null, recent: [], limit: 1 };
const setup = () => {
  const props = {
    status: boot,
    failure: "",
    onRetry: vi.fn().mockResolvedValue(undefined),
    onRestored: vi.fn(),
    onResumeProfile: vi.fn().mockResolvedValue(ready),
    onProfileResumed: vi.fn(),
    onHelp: vi.fn(),
    work: { status: vi.fn().mockResolvedValue(idle), cancel: vi.fn() },
  };
  const view = render(<StartupRecovery {...props} />);
  return { props, ...view };
};
async function resume() {
  const button = await screen.findByRole("button", {
    name: "Resume selected profile",
  });
  await waitFor(() => expect(button.hasAttribute("disabled")).toBe(false));
  fireEvent.click(button);
}
it("discovers the recorded profile without auto-confirming or inventing a different disposition", async () => {
  const s = setup();
  await screen.findByText(pending.profile.displayPath);
  expect(s.props.onResumeProfile).not.toHaveBeenCalled();
  await resume();
  await waitFor(() =>
    expect(s.props.onProfileResumed).toHaveBeenCalledWith(ready),
  );
  expect(s.props.onResumeProfile).toHaveBeenCalledExactlyOnceWith(
    pending.token,
    "recovery-copy",
  );
  expect(s.props.onRetry).not.toHaveBeenCalled();
});
it("retains exact token and decision after a lost response and keeps Help available", async () => {
  const s = setup();
  s.props.onResumeProfile.mockRejectedValueOnce(
    new Error("PROFILE_SELECTION_RECOVERY_REQUIRED: response lost"),
  );
  await resume();
  await screen.findByText(/response lost/);
  s.rerender(
    <StartupRecovery
      {...s.props}
      status={{
        ...boot,
        pendingProfileSwitch: {
          ...pending,
          token: "new-unrelated-token",
          disposition: "discard",
        },
      }}
    />,
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Help and local diagnostics" }),
  );
  expect(s.props.onHelp).toHaveBeenCalledOnce();
  await resume();
  await waitFor(() => expect(s.props.onResumeProfile).toHaveBeenCalledTimes(2));
  expect(s.props.onResumeProfile.mock.calls).toEqual([
    [pending.token, "recovery-copy"],
    [pending.token, "recovery-copy"],
  ]);
});
it("never substitutes discard when recorded proof is unavailable", async () => {
  const s = setup();
  s.props.onResumeProfile.mockRejectedValue(
    new Error("PROFILE_SELECTION_PROOF_REQUIRED: authorization unavailable"),
  );
  await resume();
  expect((await screen.findByRole("alert")).textContent).toContain(
    "PROFILE_SELECTION_PROOF_REQUIRED",
  );
  expect(s.props.onProfileResumed).not.toHaveBeenCalled();
  expect(s.props.onResumeProfile).toHaveBeenCalledExactlyOnceWith(
    pending.token,
    "recovery-copy",
  );
  expect(screen.queryByRole("checkbox", { name: /discard/i })).toBeNull();
});
it("does not resume before its native worker finishes and keeps the command unsettled until delivery", async () => {
  const s = setup();
  let resolve!: (value: typeof ready) => void;
  s.props.onResumeProfile.mockImplementation(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  await resume();
  expect(s.props.onProfileResumed).not.toHaveBeenCalled();
  expect(
    screen
      .getByRole("button", { name: "Resume selected profile" })
      .hasAttribute("disabled"),
  ).toBe(true);
  fireEvent.click(
    screen.getByRole("button", { name: "Help and local diagnostics" }),
  );
  expect(s.props.onHelp).toHaveBeenCalledOnce();
  await act(async () => resolve(ready));
  expect(s.props.onProfileResumed).toHaveBeenCalledWith(ready);
});

it("keeps the exact resume disabled until an observed native worker has a terminal record, not only active=null", async () => {
  const s = setup();
  let reject!: (reason: unknown) => void;
  s.props.onResumeProfile.mockImplementationOnce(
    () =>
      new Promise((_done, fail) => {
        reject = fail;
      }),
  );
  await resume();
  const job = {
    id: "retired-switch",
    contextId: "old-context",
    profileGeneration: "profile",
    kind: "switch-profile",
    phase: "loading-selection-request",
    state: "running",
    completed: 0,
    total: null,
    cancelRequested: false,
    message: null,
  };
  s.props.work.status.mockResolvedValue({
    active: job,
    recent: [],
    limit: 1,
  } as never);
  const refresh = () =>
    screen.getByRole("button", { name: "Refresh recovery work status" });
  await waitFor(() => expect(refresh().hasAttribute("disabled")).toBe(false));
  fireEvent.click(refresh());
  await screen.findByText(/Loading the recorded profile selection/);
  await act(async () =>
    reject(
      new Error("PROFILE_SELECTION_RECOVERY_REQUIRED: uncertain confirmation"),
    ),
  );
  s.props.work.status.mockResolvedValue(idle);
  await waitFor(() => expect(refresh().hasAttribute("disabled")).toBe(false));
  fireEvent.click(refresh());
  await act(async () => {});
  expect(
    screen
      .getByRole("button", { name: "Resume selected profile" })
      .hasAttribute("disabled"),
  ).toBe(true);
  s.props.work.status.mockResolvedValue({
    active: null,
    recent: [{ ...job, state: "failed" }],
    limit: 1,
  } as never);
  fireEvent.click(refresh());
  await waitFor(() =>
    expect(
      screen
        .getByRole("button", { name: "Resume selected profile" })
        .hasAttribute("disabled"),
    ).toBe(false),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Resume selected profile" }),
  );
  await waitFor(() => expect(s.props.onResumeProfile).toHaveBeenCalledTimes(2));
  expect(s.props.onResumeProfile.mock.calls[1]).toEqual([
    pending.token,
    "recovery-copy",
  ]);
});
it("does not confirm again if native selection succeeded but renderer navigation failed", async () => {
  const s = setup();
  s.props.onProfileResumed.mockImplementation(() => {
    throw new Error("Renderer navigation failed");
  });
  await resume();
  await screen.findByText("Error: Renderer navigation failed");
  const button = screen.getByRole("button", {
    name: "Resume selected profile",
  });
  expect(button.hasAttribute("disabled")).toBe(true);
  fireEvent.click(button);
  expect(s.props.onResumeProfile).toHaveBeenCalledOnce();
});
