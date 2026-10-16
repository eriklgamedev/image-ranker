// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { CloseGuard } from "../../src/components/CloseGuard";
const mocks = vi.hoisted(() => ({
  subscribeCloseRequested: vi.fn(),
  confirmClose: vi.fn(),
  cancelClose: vi.fn(),
  exportSessionCopy: vi.fn(),
  cancelScan: vi.fn(),
  cancelOperation: vi.fn(),
}));
vi.mock("../../src/native", () => ({ desktop: true, api: mocks }));
beforeEach(() => {
  vi.resetAllMocks();
  mocks.subscribeCloseRequested.mockResolvedValue(() => {});
  mocks.confirmClose.mockResolvedValue(undefined);
  mocks.cancelClose.mockResolvedValue(undefined);
  mocks.cancelScan.mockResolvedValue(undefined);
  mocks.cancelOperation.mockResolvedValue(undefined);
});
afterEach(cleanup);
const pending = () => {
  let resolve!: () => void;
  const promise = new Promise<void>((accept) => {
    resolve = accept;
  });
  return { promise, resolve };
};
function mount(overrides: Partial<Parameters<typeof CloseGuard>[0]> = {}) {
  const props = {
    freeze: vi.fn(),
    flush: vi.fn().mockResolvedValue(undefined),
    retry: vi.fn().mockResolvedValue(undefined),
    snapshot: () => ({ run: { history: { length: 9 } } }),
    activeWork: () => false,
    ...overrides,
  };
  render(<CloseGuard {...props} />);
  return props;
}
async function request(token = "close-1") {
  await waitFor(() =>
    expect(mocks.subscribeCloseRequested).toHaveBeenCalledTimes(1),
  );
  await act(async () =>
    mocks.subscribeCloseRequested.mock.calls[0][0]({ token }),
  );
}
describe("native close handshake", () => {
  it("cancels a close while a queued save waits and does not close when its late acknowledgment arrives", async () => {
    const write = pending();
    const props = mount({ flush: () => write.promise });
    await request();
    fireEvent.click(screen.getByRole("button", { name: "Stay open" }));
    await waitFor(() => expect(props.freeze).toHaveBeenLastCalledWith(false));
    await act(async () => write.resolve());
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    expect(mocks.cancelClose).toHaveBeenCalledWith("close-1");
  });
  it("freezes synchronously and waits for all queued writes before confirming saved close", async () => {
    const first = pending(),
      second = pending();
    const props = mount({
      flush: async () => {
        await first.promise;
        await second.promise;
      },
    });
    await request();
    expect(props.freeze).toHaveBeenCalledWith(true);
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    await act(async () => first.resolve());
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    await act(async () => second.resolve());
    expect(mocks.confirmClose).toHaveBeenCalledWith("close-1", "saved");
  });
  it("keeps a failed save open and retries the same pending state before closing", async () => {
    const flush = vi
      .fn()
      .mockRejectedValueOnce(new Error("Disk full"))
      .mockResolvedValueOnce(undefined);
    const props = mount({ flush });
    await request();
    await screen.findByText("Error: Disk full");
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    fireEvent.click(
      screen.getByRole("button", { name: "Retry save and close" }),
    );
    await waitFor(() =>
      expect(mocks.confirmClose).toHaveBeenCalledWith("close-1", "saved"),
    );
    expect(props.retry).toHaveBeenCalledTimes(1);
  });
  it("does not close after a recovery-copy cancellation or export failure", async () => {
    mount({
      flush: async () => {
        throw new Error("Save failed");
      },
    });
    mocks.exportSessionCopy
      .mockResolvedValueOnce(null)
      .mockRejectedValueOnce(new Error("Recovery drive full"));
    await request();
    fireEvent.click(
      screen.getByRole("button", { name: "Save recovery copy and close" }),
    );
    await screen.findByText(
      "Recovery copy cancelled. The app remains open and your pending work stays in memory.",
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Save recovery copy and close" }),
    );
    await screen.findByText("Error: Recovery drive full");
    expect(mocks.confirmClose).not.toHaveBeenCalled();
  });
  it("closes through recovery-copy disposition without claiming the failed save succeeded", async () => {
    const props = mount({
      flush: async () => {
        throw new Error("Save failed");
      },
    });
    mocks.exportSessionCopy.mockResolvedValue("D:/Recovery/new.json");
    await request();
    fireEvent.click(
      screen.getByRole("button", { name: "Save recovery copy and close" }),
    );
    await waitFor(() =>
      expect(mocks.confirmClose).toHaveBeenCalledWith(
        "close-1",
        "recovery-copy",
      ),
    );
    expect(mocks.exportSessionCopy).toHaveBeenCalledWith(props.snapshot());
    expect(props.retry).not.toHaveBeenCalled();
    expect(mocks.confirmClose).not.toHaveBeenCalledWith("close-1", "saved");
  });
  it("requires explicit discard consent and preserves the native close disposition", async () => {
    mount({
      flush: async () => {
        throw new Error("Save failed");
      },
    });
    await request();
    const discard = screen.getByRole("button", {
      name: "Discard unsaved changes and close",
    });
    expect(discard.hasAttribute("disabled")).toBe(true);
    fireEvent.click(
      screen.getByLabelText(
        "I accept losing changes without a saved acknowledgment.",
      ),
    );
    fireEvent.click(discard);
    await waitFor(() =>
      expect(mocks.confirmClose).toHaveBeenCalledWith("close-1", "discard"),
    );
  });
  it("cancels close without losing pending work or leaving editing frozen", async () => {
    const props = mount({
      flush: async () => {
        throw new Error("Save failed");
      },
    });
    await request();
    fireEvent.click(screen.getByRole("button", { name: "Stay open" }));
    await waitFor(() => expect(props.freeze).toHaveBeenLastCalledWith(false));
    expect(mocks.cancelClose).toHaveBeenCalledWith("close-1");
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).toBeNull();
  });
  it("requests safe cancellation for file work and only closes after the boundary is reached", async () => {
    let active = true;
    mount({ activeWork: () => active });
    await request();
    expect(mocks.confirmClose).not.toHaveBeenCalled();
    fireEvent.click(
      screen.getByRole("button", { name: "Stop active work safely" }),
    );
    await screen.findByText(
      "Stop requested. Retry closing after the active work has reached a safe boundary.",
    );
    expect(mocks.cancelScan).toHaveBeenCalledTimes(1);
    expect(mocks.cancelOperation).toHaveBeenCalledTimes(1);
    active = false;
    fireEvent.click(
      screen.getByRole("button", { name: "Retry save and close" }),
    );
    await waitFor(() =>
      expect(mocks.confirmClose).toHaveBeenCalledWith("close-1", "saved"),
    );
  });
});
