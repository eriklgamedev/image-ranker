// @vitest-environment jsdom
import { beforeEach, afterEach, describe, it, expect, vi } from "vitest";
import {
  render,
  screen,
  fireEvent,
  waitFor,
  cleanup,
  act,
} from "@testing-library/react";
import { HelpDialog } from "../../src/components/HelpDialog";
import App from "../../src/App";
import type { AppInfo, DiagnosticsPreview } from "../../src/native";
const mocks = vi.hoisted(() => ({
  recoveryStorageStatus: vi.fn(),
  recoveryWorkStatus: vi.fn(),
  cancelRecoveryWork: vi.fn(),
  protectionStatus: vi.fn(),
  appInfo: vi.fn(),
  licenseNotices: vi.fn(),
  storageLocations: vi.fn(),
  openStorageLocation: vi.fn(),
  previewDiagnostics: vi.fn(),
  exportDiagnostics: vi.fn(),
  getBootStatus: vi.fn(),
  loadSession: vi.fn(),
  loadSessionState: vi.fn(),
  saveSession: vi.fn(),
  subscribeCloseRequested: vi.fn(),
  imageWorkStatus: vi.fn(),
  cancelImageWork: vi.fn(),
}));
vi.mock("../../src/native", () => ({
  desktop: true,
  api: mocks,
  previewUrl: (image: { previewPath: string }) => image.previewPath,
}));
const info: AppInfo = {
  name: "Image Ranker",
  version: "0.1.0-test",
  platform: "windows",
  architecture: "x86_64",
  buildType: "debug",
  testDriverEnabled: true,
  applicationLicense: null,
  decode: {
    formats: ["JPEG", "TIFF (qualified first-page variants)"],
    maxInputBytes: 1000000,
    maxPixels: 40000000,
    maxEdge: 32768,
    maxDecodeBytes: 200000000,
    maxIccBytes: 1000000,
    channelBits: [8],
    gif: "first-frame",
    tiff: "first-page",
    orientation: "exif-normalized",
    color: "embedded-profile-to-srgb",
  },
};
const diagnostic: DiagnosticsPreview = {
  token: "frozen-token-1",
  createdAt: "2026-09-29T00:00:00Z",
  redacted: true,
  text: "Image Ranker\nStore: [redacted]\n<script>not executable</script>\n",
  sizeBytes: 73,
  limitations: ["Images and recovery backup bytes excluded."],
};
beforeEach(() => {
  vi.resetAllMocks();
  mocks.recoveryWorkStatus.mockResolvedValue({ active: null, recent: [], limit: 1 });
  mocks.protectionStatus.mockResolvedValue({ state: "unavailable", capacity: null, message: "SUPPORT_BUSY: Retry the protection check." });
  mocks.recoveryStorageStatus.mockResolvedValue({ selectedRequestId: null, selectedBranchId: null, activationState: "original", usedBranches: 0, usedBytes: 0, limits: { branches: 8, bytes: 2147483648, requests: 1024 }, requests: [], warnings: [] });
  mocks.appInfo.mockResolvedValue(info);
  mocks.licenseNotices.mockResolvedValue([
    {
      id: "react",
      component: "React",
      version: "19.3.0",
      spdx: "MIT",
      text: "Fixture notice with required license text.",
    },
  ]);
  mocks.storageLocations.mockResolvedValue([
    {
      kind: "profile",
      label: "Profile data",
      displayPath: "C:\\known-profile",
      available: true,
    },
    {
      kind: "operations",
      label: "Operation journals",
      displayPath: "E:\\offline",
      available: false,
    },
  ]);
  mocks.openStorageLocation.mockResolvedValue(undefined);
  mocks.previewDiagnostics.mockResolvedValue(diagnostic);
  mocks.exportDiagnostics.mockResolvedValue(null);
  mocks.getBootStatus.mockResolvedValue({
    mode: "recovery",
    state: "read-only-recovery",
    issues: [
      { code: "CORRUPT_STORE", message: "Saved history could not be parsed." },
    ],
    canRestoreBackup: false,
    pendingOperations: 0,
    trustedOperationsAvailable: false,
  });
  mocks.subscribeCloseRequested.mockResolvedValue(() => {});
  mocks.imageWorkStatus.mockResolvedValue({
    active: 0,
    limit: 18,
    jobs: [],
    decoder: {
      active: 0,
      pending: 0,
      reservedBytes: 0,
      liveOutputs: 0,
      outputReservedBytes: 0,
      unavailable: false,
      workerPid: null,
      stage: null,
      recoveryAction: null,
    },
  });
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});
describe("bundled help and local support", () => {
  it("offers a manual protection check in Help without reading on dialog open", async () => {
    render(<HelpDialog onClose={() => {}} />);
    await screen.findByText(/Version 0.1.0-test/);
    expect(mocks.protectionStatus).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Check protection capacity" }));
    await screen.findByText("Protection capacity unavailable");
    expect(mocks.protectionStatus).toHaveBeenCalledOnce();
    expect(screen.queryByLabelText("Observed protection capacity")).toBeNull();
    expect(mocks.previewDiagnostics).not.toHaveBeenCalled();
  });
  it("exports exactly the displayed capture and distinguishes picker cancellation from success", async () => {
    const view = render(<HelpDialog onClose={() => {}} />);
    await screen.findByText(/Version 0.1.0-test/);
    expect(
      screen.getByText(/TIFF \(qualified first-page variants\)/),
    ).toBeTruthy();
    fireEvent.click(
      screen.getByRole("button", { name: "Preview diagnostics" }),
    );
    const preview = await screen.findByLabelText(
      "Exact diagnostic export preview",
    );
    expect(preview.textContent).toBe(diagnostic.text);
    expect(view.container.querySelector("script")).toBeNull();
    fireEvent.click(
      screen.getByRole("button", { name: "Save displayed diagnostics" }),
    );
    await screen.findByText("Export cancelled. No diagnostic file was saved.");
    expect(mocks.exportDiagnostics).toHaveBeenCalledWith(diagnostic.token);
    expect(mocks.previewDiagnostics).toHaveBeenCalledTimes(1);
    mocks.exportDiagnostics.mockResolvedValueOnce("C:\\support\\report.txt");
    fireEvent.click(
      screen.getByRole("button", { name: "Save displayed diagnostics" }),
    );
    await screen.findByText(
      "Displayed diagnostics saved: C:\\support\\report.txt",
    );
    expect(mocks.previewDiagnostics).toHaveBeenCalledTimes(1);
  });
  it("keeps the captured report after export failure and invalidates it when refreshing", async () => {
    mocks.exportDiagnostics.mockRejectedValueOnce(new Error("DISK_FULL"));
    render(<HelpDialog onClose={() => {}} />);
    fireEvent.click(
      screen.getByRole("button", { name: "Preview diagnostics" }),
    );
    await screen.findByLabelText("Exact diagnostic export preview");
    fireEvent.click(
      screen.getByRole("button", { name: "Save displayed diagnostics" }),
    );
    await screen.findByText(/DISK_FULL/);
    expect(
      screen.getByLabelText("Exact diagnostic export preview").textContent,
    ).toBe(diagnostic.text);
    mocks.previewDiagnostics.mockRejectedValueOnce(
      new Error("Preview unavailable"),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Refresh diagnostic preview" }),
    );
    await screen.findByText(/Preview unavailable/);
    expect(
      screen.queryByRole("button", { name: "Save displayed diagnostics" }),
    ).toBeNull();
  });
  it("opens only available native location kinds and blocks closing during export", async () => {
    const close = vi.fn();
    let resolve!: (path: string | null) => void;
    mocks.exportDiagnostics.mockImplementation(
      () =>
        new Promise((result) => {
          resolve = result;
        }),
    );
    render(<HelpDialog onClose={close} />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Open Profile data" }),
    );
    await waitFor(() =>
      expect(mocks.openStorageLocation).toHaveBeenCalledWith("profile"),
    );
    expect(
      screen
        .getByRole("button", { name: "Open Operation journals" })
        .hasAttribute("disabled"),
    ).toBe(true);
    fireEvent.click(
      screen.getByRole("button", { name: "Preview diagnostics" }),
    );
    await screen.findByLabelText("Exact diagnostic export preview");
    fireEvent.click(
      screen.getByRole("button", { name: "Save displayed diagnostics" }),
    );
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(close).not.toHaveBeenCalled();
    await act(async () => resolve(null));
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(close).toHaveBeenCalledTimes(1);
  });
  it("allows diagnostics during restricted startup without loading or writing the damaged session", async () => {
    mocks.appInfo.mockRejectedValueOnce(new Error("Metadata unavailable"));
    render(<App />);
    await screen.findByText("Your workspace needs recovery");
    expect(
      screen.getByRole("button", { name: "Settings" }).hasAttribute("disabled"),
    ).toBe(true);
    await act(async () => {});
    await waitFor(() => expect(screen.getByRole("button", { name: "Help and local diagnostics" }).hasAttribute("disabled")).toBe(false));
    fireEvent.click(
      screen.getByRole("button", { name: "Help and local diagnostics" }),
    );
    await screen.findByText(/Metadata unavailable/);
    fireEvent.click(
      screen.getByRole("button", { name: "Preview diagnostics" }),
    );
    await screen.findByLabelText("Exact diagnostic export preview");
    fireEvent.click(
      screen.getByRole("button", { name: "Save displayed diagnostics" }),
    );
    await screen.findByText("Export cancelled. No diagnostic file was saved.");
    expect(mocks.loadSession).not.toHaveBeenCalled();
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
  it("exposes independent image cancellation from restricted-startup Help", async () => {
    mocks.imageWorkStatus.mockResolvedValue({
      active: 1,
      limit: 18,
      jobs: [
        {
          requestId: "retiring",
          contextId: "c",
          kind: "preview",
          phase: "decoding",
          cancelled: false,
        },
      ],
      decoder: {
        active: 1,
        pending: 0,
        reservedBytes: 1048576,
        liveOutputs: 0,
        outputReservedBytes: 0,
        unavailable: false,
        workerPid: 321,
        stage: "decode",
        recoveryAction: null,
      },
    });
    mocks.cancelImageWork.mockResolvedValue(1);
    render(<App />);
    await screen.findByText("Your workspace needs recovery");
    await act(async () => {});
    await waitFor(() => expect(screen.getByRole("button", { name: "Help and local diagnostics" }).hasAttribute("disabled")).toBe(false));
    fireEvent.click(
      screen.getByRole("button", { name: "Help and local diagnostics" }),
    );
    await screen.findByText("1 active image requests · 18 request limit");
    fireEvent.click(screen.getByRole("button", { name: "Cancel image work" }));
    await screen.findByText(/Work remains reserved until it exits/);
    expect(mocks.cancelImageWork).toHaveBeenCalledTimes(1);
    expect(mocks.loadSessionState).not.toHaveBeenCalled();
    expect(mocks.saveSession).not.toHaveBeenCalled();
  });
});
