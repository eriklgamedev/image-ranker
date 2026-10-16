// @vitest-environment jsdom
import { StrictMode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ProtectionStatusPanel } from "../../src/components/ProtectionStatusPanel";
import { StartupRecovery } from "../../src/components/StartupRecovery";
import type { ProtectionStatus, RegistryCapacity } from "../../src/native";

const mocks = vi.hoisted(() => ({ desktop: true, protectionStatus: vi.fn() }));
vi.mock("../../src/native", () => ({
  get desktop() {
    return mocks.desktop;
  },
  api: mocks,
}));
const capacity: RegistryCapacity = {
  hardBytes: 16777216,
  hardClaims: 100000,
  ordinaryByteLimit: 14680064,
  ordinaryClaimLimit: 99968,
  usedBytes: 4096,
  usedClaims: 20,
  reservedCompletionBytes: 1024,
  reservedCompletionClaims: 1,
  emergencyByteAllowance: 1048576,
  emergencyClaimAllowance: 16,
  maintenanceByteAllowance: 1048576,
  maintenanceClaimAllowance: 16,
  ordinaryBytesRemaining: 14674944,
  ordinaryClaimsRemaining: 99947,
  publicationBytesRemaining: 15723520,
  publicationClaimsRemaining: 99963,
};
const ready: ProtectionStatus = { state: "ready", capacity, message: null };
function deferred<T>() {
  let resolve!: (value: T) => void, reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
async function check(name = "Check protection capacity") {
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name }));
  });
}
function metric(label: string, bytes: number, claims: number) {
  expect(screen.getByText(label).nextElementSibling?.textContent).toBe(
    `${bytes.toLocaleString()} bytes · ${claims.toLocaleString()} claims`,
  );
}
beforeEach(() => {
  vi.resetAllMocks();
  mocks.desktop = true;
  mocks.protectionStatus.mockResolvedValue(ready);
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("on-demand protection capacity", () => {
  it("does not read automatically, including StrictMode or elapsed time", async () => {
    vi.useFakeTimers();
    render(
      <StrictMode>
        <ProtectionStatusPanel />
      </StrictMode>,
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10000);
    });
    expect(mocks.protectionStatus).not.toHaveBeenCalled();
    expect(screen.getByText("Capacity has not been checked.")).toBeTruthy();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("shows the native accounting values without implying operation admission", async () => {
    render(<ProtectionStatusPanel />);
    await check();
    expect(screen.getByRole("status").textContent).toBe(
      "Protection capacity available",
    );
    metric("Recorded usage", capacity.usedBytes, capacity.usedClaims);
    metric("Hard limit", capacity.hardBytes, capacity.hardClaims);
    metric(
      "Ordinary admission ceiling",
      capacity.ordinaryByteLimit,
      capacity.ordinaryClaimLimit,
    );
    metric(
      "Ordinary headroom",
      capacity.ordinaryBytesRemaining,
      capacity.ordinaryClaimsRemaining,
    );
    metric(
      "Recovery publication headroom",
      capacity.publicationBytesRemaining,
      capacity.publicationClaimsRemaining,
    );
    metric(
      "Reserved completions",
      capacity.reservedCompletionBytes,
      capacity.reservedCompletionClaims,
    );
    metric(
      "Emergency allowance",
      capacity.emergencyByteAllowance,
      capacity.emergencyClaimAllowance,
    );
    metric(
      "Maintenance allowance",
      capacity.maintenanceByteAllowance,
      capacity.maintenanceClaimAllowance,
    );
    expect(
      screen.getByText(/does not guarantee that the next operation fits/),
    ).toBeTruthy();
    expect(screen.getByText(/separate preview and approval flow/)).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: /delete|reset|migrate/i }),
    ).toBeNull();
  });

  it.each([
    ["ordinary-limit", "Ordinary admission limit reached"],
    ["recovery-limit", "Recovery publication limit reached"],
  ] as const)(
    "distinguishes native %s from unavailable capacity",
    async (state, label) => {
      mocks.protectionStatus.mockResolvedValue({
        state,
        capacity: {
          ...capacity,
          ordinaryBytesRemaining: 0,
          ordinaryClaimsRemaining: 0,
          ...(state === "recovery-limit"
            ? { publicationBytesRemaining: 0, publicationClaimsRemaining: 0 }
            : {}),
        },
        message: "Native capacity threshold reached.",
      });
      render(<ProtectionStatusPanel />);
      await check();
      expect(screen.getByRole("status").textContent).toBe(label);
      expect(
        screen.getByText("Native capacity threshold reached."),
      ).toBeTruthy();
      metric("Ordinary headroom", 0, 0);
      expect(
        screen.queryByText(/No current capacity reading is available/),
      ).toBeNull();
    },
  );

  it.each([null, capacity])(
    "never presents unavailable status as zero or free capacity (%j)",
    async (nativeCapacity) => {
      mocks.protectionStatus.mockResolvedValue({
        state: "unavailable",
        capacity: nativeCapacity,
        message: "Registry ownership could not be verified.",
      });
      render(<ProtectionStatusPanel />);
      await check();
      expect(screen.getByRole("status").textContent).toBe(
        "Protection capacity unavailable",
      );
      expect(
        screen.getByText(/No current capacity reading is available/),
      ).toBeTruthy();
      expect(
        screen.queryByLabelText("Observed protection capacity"),
      ).toBeNull();
      expect(screen.queryByText(/0 bytes/)).toBeNull();
    },
  );

  it("does not treat a ready response without capacity as an available balance", async () => {
    mocks.protectionStatus.mockResolvedValue({
      state: "ready",
      capacity: null,
      message: null,
    });
    render(<ProtectionStatusPanel />);
    await check();
    expect(screen.getByRole("status").textContent).toBe(
      "Protection capacity unavailable",
    );
    expect(screen.queryByLabelText("Observed protection capacity")).toBeNull();
  });

  it("removes stale values as soon as a refresh starts and exposes SUPPORT_BUSY without zeros", async () => {
    const next = deferred<ProtectionStatus>();
    render(<ProtectionStatusPanel />);
    await check();
    mocks.protectionStatus.mockReturnValueOnce(next.promise);
    fireEvent.click(
      screen.getByRole("button", { name: "Refresh protection capacity" }),
    );
    expect(screen.queryByLabelText("Observed protection capacity")).toBeNull();
    expect(screen.getByRole("status").textContent).toContain("Reading current");
    await act(async () => {
      next.reject(new Error("SUPPORT_BUSY: recovery work holds the registry"));
    });
    expect(screen.getByRole("status").textContent).toBe(
      "Protection capacity unavailable",
    );
    expect(screen.getByRole("alert").textContent).toContain("SUPPORT_BUSY");
    expect(screen.queryByLabelText("Observed protection capacity")).toBeNull();
    mocks.protectionStatus.mockResolvedValue({
      ...ready,
      capacity: { ...capacity, usedBytes: 8192 },
    });
    await check("Refresh protection capacity");
    expect(screen.queryByRole("alert")).toBeNull();
    metric("Recorded usage", 8192, capacity.usedClaims);
  });

  it("serializes duplicate clicks instead of overlapping native reads", async () => {
    const pending = deferred<ProtectionStatus>();
    mocks.protectionStatus.mockReturnValue(pending.promise);
    render(<ProtectionStatusPanel />);
    const button = screen.getByRole("button", {
      name: "Check protection capacity",
    });
    fireEvent.click(button);
    fireEvent.click(button);
    expect(mocks.protectionStatus).toHaveBeenCalledOnce();
    expect((button as HTMLButtonElement).disabled).toBe(true);
    await act(async () => {
      pending.resolve(ready);
    });
    expect(
      (
        screen.getByRole("button", {
          name: "Refresh protection capacity",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(false);
  });

  it.each(["resolve", "reject"] as const)(
    "ignores an obsolete source's late %s and requires an explicit check of the new source",
    async (settle) => {
      const pending = deferred<ProtectionStatus>();
      const oldRead = vi.fn(() => pending.promise),
        nextRead = vi
          .fn()
          .mockResolvedValue({ ...ready, state: "ordinary-limit" });
      const view = render(<ProtectionStatusPanel readStatus={oldRead} />);
      fireEvent.click(
        screen.getByRole("button", { name: "Check protection capacity" }),
      );
      view.rerender(<ProtectionStatusPanel readStatus={nextRead} />);
      expect(nextRead).not.toHaveBeenCalled();
      await act(async () => {
        if (settle === "resolve") pending.resolve(ready);
        else pending.reject(new Error("obsolete ownership failure"));
      });
      expect(screen.queryByRole("alert")).toBeNull();
      expect(
        screen.queryByLabelText("Observed protection capacity"),
      ).toBeNull();
      await check();
      expect(nextRead).toHaveBeenCalledOnce();
      expect(screen.getByRole("status").textContent).toBe(
        "Ordinary admission limit reached",
      );
    },
  );

  it("ignores an in-flight result after unmount without scheduling more reads", async () => {
    const pending = deferred<ProtectionStatus>();
    mocks.protectionStatus.mockReturnValue(pending.promise);
    const view = render(<ProtectionStatusPanel />);
    fireEvent.click(
      screen.getByRole("button", { name: "Check protection capacity" }),
    );
    view.unmount();
    await act(async () => {
      pending.resolve(ready);
    });
    expect(screen.queryByLabelText("Protection capacity")).toBeNull();
    expect(mocks.protectionStatus).toHaveBeenCalledOnce();
  });

  it("allows a requested check from restricted startup while a recovery operation remains pending", async () => {
    const recovery = deferred<void>();
    const onRetry = vi.fn(() => recovery.promise);
    render(
      <StartupRecovery
        status={{ mode: "recovery", issues: [], canRestoreBackup: false }}
        failure="Session unavailable"
        onRetry={onRetry}
        onRestored={() => {}}
      />,
    );
    expect(mocks.protectionStatus).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Retry loading" }));
    expect(
      (
        screen.getByRole("button", {
          name: "Retry loading",
        }) as HTMLButtonElement
      ).disabled,
    ).toBe(true);
    await check();
    expect(mocks.protectionStatus).toHaveBeenCalledOnce();
    expect(screen.getByRole("status").textContent).toBe(
      "Protection capacity available",
    );
    await act(async () => {
      recovery.resolve();
    });
  });

  it("does not offer native accounting in browser demo", () => {
    mocks.desktop = false;
    render(<ProtectionStatusPanel />);
    expect(screen.queryByLabelText("Protection capacity")).toBeNull();
    expect(mocks.protectionStatus).not.toHaveBeenCalled();
  });
});
