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
import { afterEach, describe, expect, it, vi } from "vitest";
import { OperationRestartReview } from "../../src/components/OperationRestartReview";
import type { OperationRestartPreview } from "../../src/native";

const review: OperationRestartPreview = {
  token: "review-A",
  requestId: "retained-worker-A",
  savedRevision: 17,
  pendingActions: 2,
  rendererProposal: true,
  copyReady: false,
  copyPath: null,
  warnings: [],
};
const copied = {
  ...review,
  copyReady: true,
  copyPath: "C:/Recovery/retained.json",
};
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => {
    resolve = yes;
  });
  return { promise, resolve };
}
function fixture() {
  return {
    commands: {
      prepare: vi.fn().mockResolvedValue(review),
      exportCopy: vi.fn().mockResolvedValue(copied),
      confirm: vi.fn().mockResolvedValue(undefined),
      cancel: vi.fn().mockResolvedValue(undefined),
    },
    flush: vi.fn().mockResolvedValue(undefined),
    snapshot: vi.fn().mockReturnValue({ pending: ["preserve"] }),
    generation: 1,
  };
}
async function prepare() {
  fireEvent.click(
    screen.getByRole("button", { name: "Preserve work and review restart" }),
  );
  await screen.findByText("Verified saved revision");
}
async function copy() {
  fireEvent.click(
    screen.getByRole("button", { name: "Save restart recovery copy" }),
  );
  await screen.findByText("Verified for this exact review");
}
afterEach(cleanup);

describe("restart preservation command lifetime", () => {
  it("StrictMode does not prepare automatically and duplicate clicks cannot overlap admission", async () => {
    const props = fixture(),
      pending = deferred<OperationRestartPreview>();
    props.commands.prepare.mockReturnValue(pending.promise);
    render(
      <StrictMode>
        <OperationRestartReview {...props} />
      </StrictMode>,
    );
    expect(props.commands.prepare).not.toHaveBeenCalled();
    const button = screen.getByRole("button", {
      name: "Preserve work and review restart",
    });
    fireEvent.click(button);
    fireEvent.click(button);
    await waitFor(() => expect(props.commands.prepare).toHaveBeenCalledOnce());
    expect(
      screen
        .getByRole("button", { name: "Preparing restart review…" })
        .hasAttribute("disabled"),
    ).toBe(true);
    await act(async () => pending.resolve(review));
    expect(
      screen
        .getByRole("button", { name: "Close for restart" })
        .hasAttribute("disabled"),
    ).toBe(true);
  });
  it("does not admit a preparation after its queue wait outlives the mounted review", async () => {
    const props = fixture(),
      pending = deferred<void>();
    props.flush.mockReturnValue(pending.promise);
    const view = render(<OperationRestartReview {...props} />);
    fireEvent.click(
      screen.getByRole("button", { name: "Preserve work and review restart" }),
    );
    view.unmount();
    await act(async () => pending.resolve());
    expect(props.commands.prepare).not.toHaveBeenCalled();
  });
  it("a late old-generation copy cannot authorize closing in a remounted review", async () => {
    const props = fixture(),
      pending = deferred<OperationRestartPreview>();
    props.commands.exportCopy.mockReturnValue(pending.promise);
    const view = render(<OperationRestartReview key={1} {...props} />);
    await prepare();
    fireEvent.click(
      screen.getByRole("button", { name: "Save restart recovery copy" }),
    );
    view.rerender(<OperationRestartReview key={2} {...props} generation={2} />);
    await act(async () => pending.resolve(copied));
    expect(
      screen.queryByRole("button", { name: "Close for restart" }),
    ).toBeNull();
    expect(screen.queryByText(/Recovery copy location/)).toBeNull();
    expect(props.commands.confirm).not.toHaveBeenCalled();
    expect(props.commands.cancel).not.toHaveBeenCalled();
    expect(props.commands.prepare).toHaveBeenCalledOnce();
  });
  it("incomplete native copy receipts remain an error and never enable closing", async () => {
    const props = fixture();
    props.commands.exportCopy.mockResolvedValue({ ...copied, copyPath: null });
    render(<OperationRestartReview {...props} />);
    await prepare();
    fireEvent.click(
      screen.getByRole("button", { name: "Save restart recovery copy" }),
    );
    await screen.findByText(/native restart review response is incomplete/);
    expect(
      screen
        .getByRole("button", { name: "Close for restart" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(props.commands.confirm).not.toHaveBeenCalled();
  });
  it("native busy refusal retains the exact copy and frozen renderer expectation for a later retry", async () => {
    const props = fixture(),
      snapshot = { pending: ["first"] };
    props.snapshot.mockReturnValue(snapshot);
    props.commands.confirm.mockRejectedValueOnce(
      new Error("OPERATION_RESTART_BUSY: another owned worker has not retired"),
    );
    render(<OperationRestartReview {...props} />);
    await prepare();
    snapshot.pending.push("later");
    await copy();
    fireEvent.click(screen.getByRole("button", { name: "Close for restart" }));
    await screen.findByText(/another owned worker has not retired/);
    expect(
      screen
        .getByRole("button", { name: "Cancel restart review" })
        .hasAttribute("disabled"),
    ).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Close for restart" }));
    await screen.findByText(/Native close has been requested/);
    expect(props.commands.confirm.mock.calls).toEqual([
      ["review-A", { pending: ["first"] }],
      ["review-A", { pending: ["first"] }],
    ]);
    expect(props.commands.prepare).toHaveBeenCalledOnce();
  });
  it("a rejected copy proof cannot remain displayed as current close authorization", async () => {
    const props = fixture();
    props.commands.confirm.mockRejectedValueOnce(
      new Error("OPERATION_RESTART_COPY_REQUIRED: receipt no longer qualifies"),
    );
    render(<OperationRestartReview {...props} />);
    await prepare();
    await copy();
    fireEvent.click(screen.getByRole("button", { name: "Close for restart" }));
    await screen.findByText(/receipt no longer qualifies/);
    expect(screen.queryByText("Verified for this exact review")).toBeNull();
    expect(
      screen
        .getByRole("button", { name: "Close for restart" })
        .hasAttribute("disabled"),
    ).toBe(true);
    await copy();
    expect(props.commands.exportCopy.mock.calls).toEqual([
      ["review-A"],
      ["review-A"],
    ]);
  });
  it("failed re-preparation keeps the previous exact review available for explicit cancellation", async () => {
    const props = fixture();
    props.commands.confirm.mockRejectedValueOnce(
      new Error("OPERATION_RESTART_EXPECTATION_CHANGED: review again"),
    );
    props.commands.prepare.mockRejectedValueOnce(
      new Error("CAPACITY_LIMIT: first review rejected"),
    );
    render(<OperationRestartReview {...props} />);
    fireEvent.click(
      screen.getByRole("button", { name: "Preserve work and review restart" }),
    );
    await screen.findByText(/first review rejected/);
    await prepare();
    await copy();
    fireEvent.click(screen.getByRole("button", { name: "Close for restart" }));
    await screen.findByRole("button", { name: "Review retained work again" });
    props.commands.prepare.mockRejectedValueOnce(
      new Error("OPERATION_RESTART_BUSY: new review not admitted"),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Review retained work again" }),
    );
    await screen.findByText(/new review not admitted/);
    expect(screen.getByText("17")).toBeTruthy();
    expect(
      screen
        .getByRole("button", { name: "Close for restart" })
        .hasAttribute("disabled"),
    ).toBe(true);
    fireEvent.click(
      screen.getByRole("button", { name: "Cancel restart review" }),
    );
    await screen.findByText(/Restart review cancelled/);
    expect(props.commands.cancel).toHaveBeenCalledExactlyOnceWith("review-A");
  });
});
