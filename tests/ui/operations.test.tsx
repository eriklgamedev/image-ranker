// @vitest-environment jsdom
import { beforeEach, afterEach, describe, it, expect, vi } from "vitest";
import {
  render,
  screen,
  fireEvent,
  waitFor,
  cleanup,
  within,
  act,
} from "@testing-library/react";
import {
  OperationDialog,
  OperationHistory,
} from "../../src/components/OperationDialog";
import { demoReport } from "../../src/components/demo";
import type {
  Operation,
  OperationProgress,
  PlanRequest,
} from "../../src/native";
const mocks = vi.hoisted(() => ({
  planOperation: vi.fn(),
  commitOperation: vi.fn(),
  chooseDestination: vi.fn(),
  listOperations: vi.fn(),
  undoOperation: vi.fn(),
  recoverOperation: vi.fn(),
  subscribeOperationProgress: vi.fn(),
  cancelOperation: vi.fn(),
  openFolder: vi.fn(),
  planCleanup: vi.fn(),
  cleanupHistory: vi.fn(),
}));
vi.mock("../../src/native", () => ({ desktop: true, api: mocks }));
const images = demoReport().images,
  items = new Map(images.map((image) => [image.id, image])),
  order = images.map((image) => image.id);
function plan(request?: PlanRequest): Operation {
  return {
    id: "operation-1",
    kind: request?.kind || "rename",
    status: "planned",
    rows: (request?.imageIds || order.slice(0, 2)).map((imageId, index) => ({
      imageId,
      from: `source/${imageId}.jpg`,
      to: `destination/${String(request?.ranks[index] || index + 1).padStart(3, "0")}.jpg`,
      temporary: `source/.temporary-${index}`,
      rank: request?.ranks[index] || index + 1,
      outputNumber: request?.ranks[index] || index + 1,
      sizeBytes: 1024,
      issue: null,
      noop: false,
    })),
    errors: [],
    completed: 0,
    manifestPath: null,
    order: 1,
    dependencies: [],
  };
}
beforeEach(() => {
  vi.resetAllMocks();
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(
    function (this: HTMLElement) {
      return this.tagName === "TR" ? 90 : 360;
    },
  );
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(900);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    function (this: HTMLElement) {
      return {
        width: 900,
        height: this.tagName === "TR" ? 90 : 360,
        x: 0,
        y: 0,
        top: 0,
        left: 0,
        right: 900,
        bottom: 360,
        toJSON() {},
      };
    },
  );
  mocks.chooseDestination.mockResolvedValue("destination");
  mocks.planOperation.mockImplementation(async (request: PlanRequest) =>
    plan(request),
  );
  mocks.subscribeOperationProgress.mockResolvedValue(() => {});
  mocks.commitOperation.mockResolvedValue({
    ...plan(),
    status: "complete",
    completed: 4,
    manifestPath: "destination/ranking.json",
  });
  mocks.listOperations.mockResolvedValue([]);
  mocks.cancelOperation.mockResolvedValue(undefined);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});
describe("reviewable file operations", () => {
  it("retains a completed group Undo while catalog saving is pending or fails, without offering Undo twice", async () => {
    const moved: Operation = {
      ...plan(),
      status: "complete",
      exfatOutputs: true,
      context: {
        sourceGroup: { parentPath: "E:\\Pictures", folderName: "Ink" },
      },
    };
    const restored = { ...moved, status: "rolled-back" };
    let rejectSave!: (reason: Error) => void;
    const saving = new Promise<void>((_resolve, reject) => {
      rejectSave = reject;
    });
    mocks.listOperations.mockResolvedValue([moved]);
    mocks.undoOperation.mockResolvedValue(restored);
    render(<OperationHistory onClose={() => {}} onComplete={() => saving} />);
    await screen.findByText("Move to subfolder · 2 files");
    expect(
      screen.getByText(/These moved images remain protected/),
    ).toBeTruthy();
    fireEvent.click(
      screen.getByRole("button", { name: "Undo file operation" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Confirm undo" }));
    await screen.findByText("rolled-back");
    expect(
      screen.queryByRole("button", { name: "Undo file operation" }),
    ).toBeNull();
    expect(
      screen
        .getByRole("button", { name: "Close dialog" })
        .hasAttribute("disabled"),
    ).toBe(true);
    await act(async () =>
      rejectSave(new Error("Could not save refreshed group")),
    );
    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toContain(
        "Could not save refreshed group",
      ),
    );
    expect(screen.queryByRole("button", { name: "Confirm undo" })).toBeNull();
    expect(mocks.undoOperation).toHaveBeenCalledOnce();
  });
  it("describes keeping a partial moved group without calling it an export", async () => {
    const pending: Operation = {
      ...plan(),
      status: "needs-recovery",
      exfatOutputs: true,
      context: {
        sourceGroup: { parentPath: "E:\\Pictures", folderName: "Ink" },
      },
    };
    mocks.listOperations.mockResolvedValue([pending]);
    render(<OperationHistory onClose={() => {}} onComplete={() => {}} />);
    fireEvent.click(
      await screen.findByRole("button", {
          name: "Keep files where they are",
      }),
    );
    expect(
      screen.getByText(/Keep every image in its current location/),
    ).toBeTruthy();
    expect(screen.queryByText(/partial export files/)).toBeNull();
    expect(mocks.recoverOperation).not.toHaveBeenCalled();
  });
  it("uses full-ranking positions for exported picks and requires an explicit reviewed preview", async () => {
    const complete = vi.fn();
    render(
      <OperationDialog
        kind="copy"
        ids={[order[1], order[8], order[11]]}
        order={order}
        items={items}
        onClose={() => {}}
        onComplete={complete}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Browse" }));
    await waitFor(() =>
      expect(
        (screen.getByLabelText("Export destination") as HTMLInputElement).value,
      ).toBe("destination"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Build preview" }));
    await screen.findByText("3 planned rows");
    expect(mocks.planOperation).toHaveBeenCalledWith(
      expect.objectContaining({
        imageIds: [order[1], order[8], order[11]],
        ranks: [2, 9, 12],
        reviewCount: 12,
        destination: "destination",
        conflict: "suffix",
        manifest: true,
      }),
    );
    const commit = screen.getByRole("button", { name: "Copy 3 images" });
    expect(commit.hasAttribute("disabled")).toBe(true);
    fireEvent.click(
      screen.getByLabelText(
        "I reviewed the full preview and approve these file changes.",
      ),
    );
    fireEvent.click(commit);
    fireEvent.click(commit);
    await screen.findByText("Operation completed");
    expect(mocks.commitOperation).toHaveBeenCalledTimes(1);
    expect(mocks.commitOperation).toHaveBeenCalledWith("operation-1");
    expect(complete).toHaveBeenCalledWith(
      expect.objectContaining({ status: "complete" }),
    );
  });
  it("blocks collisions and invalidates the entire preview when naming changes", async () => {
    const conflicted = plan();
    conflicted.rows[0].issue = "Target is occupied by an unrelated file";
    mocks.planOperation.mockResolvedValue(conflicted);
    render(
      <OperationDialog
        kind="rename"
        ids={order.slice(0, 2)}
        order={order}
        items={items}
        onClose={() => {}}
        onComplete={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Build preview" }));
    await screen.findByText("Target is occupied by an unrelated file");
    expect(
      screen
        .getByRole("button", { name: "Rename 2 images" })
        .hasAttribute("disabled"),
    ).toBe(true);
    fireEvent.change(screen.getByLabelText("Starting number"), {
      target: { value: "10" },
    });
    expect(
      screen.queryByText("Target is occupied by an unrelated file"),
    ).toBeNull();
    expect(screen.getByRole("button", { name: "Build preview" })).toBeTruthy();
    expect(mocks.commitOperation).not.toHaveBeenCalled();
  });
  it("accepts start zero and opt-in copied-byte verification for all-ranked copies", async () => {
    render(
      <OperationDialog
        kind="copy"
        scope="all"
        ids={order}
        order={order}
        items={items}
        onClose={() => {}}
        onComplete={() => {}}
      />,
    );
    fireEvent.change(screen.getByLabelText("Starting number"), {
      target: { value: "0" },
    });
    fireEvent.click(
      screen.getByLabelText(
        "Verify copied bytes by reading each output back (slower)",
      ),
    );
    fireEvent.click(screen.getByRole("button", { name: "Browse" }));
    await waitFor(() =>
      expect(
        (screen.getByLabelText("Export destination") as HTMLInputElement).value,
      ).toBe("destination"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Build preview" }));
    await screen.findByText("12 planned rows");
    expect(mocks.planOperation).toHaveBeenCalledWith(
      expect.objectContaining({
        start: 0,
        verifyCopy: true,
        imageIds: order,
        ranks: order.map((_, index) => index + 1),
        reviewCount: 12,
      }),
    );
  });
  it("does not claim completion for files-committed when manifests need recovery", async () => {
    mocks.commitOperation.mockResolvedValue({
      ...plan(),
      status: "files-committed",
      errors: ["Manifest write failed"],
      completed: 4,
    });
    const recovery = vi.fn();
    render(
      <OperationDialog
        kind="rename"
        ids={order.slice(0, 2)}
        order={order}
        items={items}
        onClose={() => {}}
        onComplete={() => {}}
        onRecovery={recovery}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Build preview" }));
    await screen.findByText("2 planned rows");
    fireEvent.click(
      screen.getByLabelText(
        "I reviewed the full preview and approve these file changes.",
      ),
    );
    fireEvent.click(screen.getByRole("button", { name: "Rename 2 images" }));
    await screen.findByText("Manifest write failed");
    expect(screen.queryByText("Operation completed")).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Rename 2 images" }),
    ).toBeNull();
    fireEvent.click(
      screen.getByRole("button", { name: "Open recovery history" }),
    );
    expect(recovery).toHaveBeenCalledTimes(1);
  });
  it("shows real progress and exposes safe-boundary cancellation during execution", async () => {
    let listener: ((operation: OperationProgress) => void) | undefined,
      finish: ((operation: Operation) => void) | undefined;
    mocks.subscribeOperationProgress.mockImplementation(async (callback) => {
      listener = callback;
      return () => {};
    });
    mocks.commitOperation.mockImplementation(
      () =>
        new Promise<Operation>((resolve) => {
          finish = resolve;
        }),
    );
    render(
      <OperationDialog
        kind="rename"
        ids={order.slice(0, 2)}
        order={order}
        items={items}
        onClose={() => {}}
        onComplete={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Build preview" }));
    await screen.findByText("2 planned rows");
    fireEvent.click(
      screen.getByLabelText(
        "I reviewed the full preview and approve these file changes.",
      ),
    );
    fireEvent.click(screen.getByRole("button", { name: "Rename 2 images" }));
    await waitFor(() => expect(listener).toBeTypeOf("function"));
    act(() =>
      listener!({
        id: "operation-1",
        sequence: 1,
        status: "staging",
        completed: 1,
        total: 2,
        rows: [],
        errors: [],
        historyRetired: false,
        snapshot: { ...plan(), status: "staging", completed: 1, sequence: 1 },
      }),
    );
    expect(screen.getByText("staging · 1 steps completed")).toBeTruthy();
    fireEvent.click(
      screen.getByRole("button", { name: "Stop at safe boundary" }),
    );
    expect(mocks.cancelOperation).toHaveBeenCalledTimes(1);
    await act(async () =>
      finish!({
        ...plan(),
        status: "needs-recovery",
        completed: 1,
        errors: ["Stopped at a safe boundary"],
      }),
    );
    expect(screen.getByRole("alert").textContent).toContain(
      "Stopped at a safe boundary",
    );
  });
});
describe("operation history recovery states", () => {
  it("explains exFAT protection and retains suffix/skip options without changing NTFS previews", async () => {
    mocks.planOperation.mockImplementation(async (request: PlanRequest) => ({
      ...plan(request),
      exfatOutputs: true,
    }));
    render(
      <OperationDialog
        kind="copy"
        ids={order.slice(0, 2)}
        order={order}
        items={items}
        onClose={() => {}}
        onComplete={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Browse" }));
    await waitFor(() =>
      expect(
        (screen.getByLabelText("Export destination") as HTMLInputElement).value,
      ).toBe("destination"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Build preview" }));
    await screen.findByText(/exFAT exports stay protected/);
    expect(
      screen.getByText(
        /Undo is available only in the current open app and profile/,
      ),
    ).toBeTruthy();
    expect(
      screen
        .getByRole("option", { name: "Replace, keeping a recovery backup" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(
      screen
        .getByRole("option", { name: "Add a numbered suffix" })
        .hasAttribute("disabled"),
    ).toBe(false);
    expect(
      screen
        .getByRole("option", { name: "Skip existing files" })
        .hasAttribute("disabled"),
    ).toBe(false);
    expect(
      screen
        .getByRole("button", { name: "Copy 2 images" })
        .hasAttribute("disabled"),
    ).toBe(true);
    fireEvent.change(screen.getByLabelText("If a file already exists"), {
      target: { value: "skip" },
    });
    expect(
      screen
        .getByRole("option", { name: "Replace, keeping a recovery backup" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(screen.getByRole("button", { name: "Build preview" })).toBeTruthy();
    mocks.chooseDestination.mockResolvedValue("ntfs-destination");
    mocks.planOperation.mockImplementation(async (request: PlanRequest) =>
      plan(request),
    );
    fireEvent.click(screen.getByRole("button", { name: "Browse" }));
    await waitFor(() =>
      expect(
        (screen.getByLabelText("Export destination") as HTMLInputElement).value,
      ).toBe("ntfs-destination"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Build preview" }));
    await screen.findByText("2 planned rows");
    expect(screen.queryByText(/exFAT exports stay protected/)).toBeNull();
    expect(
      screen
        .getByRole("option", { name: "Replace, keeping a recovery backup" })
        .hasAttribute("disabled"),
    ).toBe(false);
  });
  it("requires explicit exFAT dismissal confirmation and retains its receipt if history reload fails", async () => {
    const pending = {
      ...plan(),
      kind: "copy",
      status: "needs-recovery",
      exfatOutputs: true,
    };
    const dismissed = { ...pending, status: "abandoned", historyRetired: true };
    const complete = vi.fn();
    mocks.listOperations
      .mockResolvedValueOnce([pending])
      .mockRejectedValue(new Error("History unavailable"));
    mocks.recoverOperation.mockResolvedValue(dismissed);
    render(<OperationHistory onClose={() => {}} onComplete={complete} />);
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Keep files and dismiss export",
      }),
    );
    expect(
      screen.getByText(/Keep all completed and partial export files/),
    ).toBeTruthy();
    expect(mocks.recoverOperation).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(mocks.recoverOperation).not.toHaveBeenCalled();
    fireEvent.click(
      screen.getByRole("button", { name: "Keep files and dismiss export" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Confirm keep files and dismiss" }),
    );
    await screen.findByText("Files kept · dismissed");
    await screen.findByText("Error: History unavailable");
    expect(mocks.recoverOperation).toHaveBeenCalledExactlyOnceWith(
      pending.id,
      "abandon",
    );
    expect(complete).toHaveBeenCalledExactlyOnceWith(dismissed);
    const card = within(
      screen.getByText(pending.id).closest(".operation-card") as HTMLElement,
    );
    expect(card.queryByRole("button")).toBeNull();
    expect(
      card.getByText(/Completed and partial files were kept/),
    ).toBeTruthy();
    expect(card.queryByText("History retired")).toBeNull();
    expect(mocks.undoOperation).not.toHaveBeenCalled();
    expect(mocks.cleanupHistory).not.toHaveBeenCalled();
  });
  it("does not claim dismissal when native preservation fails", async () => {
    mocks.listOperations.mockResolvedValue([
      { ...plan(), kind: "copy", status: "needs-recovery", exfatOutputs: true },
    ]);
    mocks.recoverOperation.mockRejectedValue(
      new Error("Cannot record dismissal"),
    );
    const complete = vi.fn();
    render(<OperationHistory onClose={() => {}} onComplete={complete} />);
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Keep files and dismiss export",
      }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Confirm keep files and dismiss" }),
    );
    await screen.findByText("Error: Cannot record dismissal");
    expect(screen.queryByText("Files kept · dismissed")).toBeNull();
    expect(
      screen
        .getByRole("button", { name: "Resume operation" })
        .hasAttribute("disabled"),
    ).toBe(false);
    expect(complete).not.toHaveBeenCalled();
  });
  it("hides dismissed exports from recovery-only history and keeps NTFS recovery unchanged", async () => {
    mocks.listOperations.mockResolvedValue([
      {
        ...plan(),
        id: "dismissed-export",
        kind: "copy",
        status: "abandoned",
        exfatOutputs: true,
      },
      { ...plan(), id: "ntfs-pending", status: "needs-recovery" },
    ]);
    render(
      <OperationHistory
        recoveryOnly
        onClose={() => {}}
        onComplete={() => {}}
      />,
    );
    await screen.findByText("ntfs-pending");
    expect(screen.queryByText("dismissed-export")).toBeNull();
    expect(screen.queryByText(/exFAT exports/)).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Keep files and dismiss export" }),
    ).toBeNull();
    expect(
      screen.getByRole("button", { name: "Resume operation" }),
    ).toBeTruthy();
    expect(
      screen.getByRole("button", { name: "Roll back safely" }),
    ).toBeTruthy();
  });
  it("keeps live exFAT Undo but omits NTFS history cleanup", async () => {
    mocks.listOperations.mockResolvedValue([
      { ...plan(), kind: "copy", status: "complete", exfatOutputs: true },
    ]);
    render(<OperationHistory onClose={() => {}} onComplete={() => {}} />);
    await screen.findByRole("button", { name: "Undo file operation" });
    expect(screen.getByText(/exFAT exports stay protected/)).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: "Preview history cleanup" }),
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Keep files and dismiss export" }),
    ).toBeNull();
  });
  it("offers the correct action for each native status and confirms before reversal", async () => {
    const states = [
      "planned",
      "complete",
      "files-committed",
      "artifact-repair-only",
      "needs-recovery",
      "rolling-back",
      "rolled-back",
    ];
    mocks.listOperations.mockResolvedValue(
      states.map((status, index) => ({
        ...plan(),
        id: `operation-${status}`,
        status,
        order: index,
      })),
    );
    mocks.undoOperation.mockResolvedValue({ ...plan(), status: "rolled-back" });
    render(<OperationHistory onClose={() => {}} onComplete={() => {}} />);
    await screen.findByText("operation-complete");
    const card = (status: string) =>
      within(
        screen
          .getByText(`operation-${status}`)
          .closest(".operation-card") as HTMLElement,
      );
    expect(card("planned").queryByRole("button")).toBeNull();
    expect(
      card("rolled-back").queryByRole("button", {
        name: "Undo file operation",
      }),
    ).toBeNull();
    expect(
      card("artifact-repair-only").getByRole("button", {
        name: "Finish manifests",
      }),
    ).toBeTruthy();
    expect(
      card("files-committed").getByRole("button", { name: "Finish manifests" }),
    ).toBeTruthy();
    expect(
      card("needs-recovery").getByRole("button", { name: "Resume operation" }),
    ).toBeTruthy();
    expect(
      card("rolling-back")
        .getByRole("button", { name: "Resume operation" })
        .hasAttribute("disabled"),
    ).toBe(true);
    fireEvent.click(
      card("complete").getByRole("button", { name: "Undo file operation" }),
    );
    expect(mocks.undoOperation).not.toHaveBeenCalled();
    fireEvent.click(
      card("complete").getByRole("button", { name: "Confirm undo" }),
    );
    await waitFor(() =>
      expect(mocks.undoOperation).toHaveBeenCalledWith("operation-complete"),
    );
  });
  it("previews exact cleanup files and bytes, requires acceptance of undo loss, and retains a receipt", async () => {
    const operation = { ...plan(), status: "complete" };
    const cleanupPlan = {
      id: "cleanup-1",
      operationId: operation.id,
      files: [
        {
          path: "destination/.backup-old.jpg",
          sizeBytes: 2048,
          status: "planned",
        },
      ],
      totalBytes: 2048,
      status: "planned",
    };
    mocks.listOperations.mockResolvedValue([operation]);
    mocks.planCleanup.mockResolvedValue(cleanupPlan);
    mocks.cleanupHistory.mockResolvedValue({
      ...operation,
      historyRetired: true,
      cleanup: { ...cleanupPlan, status: "complete" },
    });
    render(<OperationHistory onClose={() => {}} onComplete={() => {}} />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Preview history cleanup" }),
    );
    await screen.findByText("destination/.backup-old.jpg");
    expect(
      screen.getByText("1 recovery files · 2.0 KB recorded size"),
    ).toBeTruthy();
    const confirm = screen.getByRole("button", {
      name: "Delete recovery files and retire history",
    });
    expect(confirm.hasAttribute("disabled")).toBe(true);
    expect(mocks.cleanupHistory).not.toHaveBeenCalled();
    fireEvent.click(
      screen.getByLabelText(
        "I reviewed these files and accept permanently losing file undo for this operation.",
      ),
    );
    fireEvent.click(confirm);
    await screen.findByText("History retired");
    expect(mocks.cleanupHistory).toHaveBeenCalledWith(
      operation.id,
      cleanupPlan.id,
    );
    expect(
      screen.queryByRole("button", { name: "Undo file operation" }),
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Preview history cleanup" }),
    ).toBeNull();
  });
  it("reloads interrupted cleanup, disables undo, and requires a new explicit preview to retry", async () => {
    const operation = { ...plan(), status: "complete" };
    const cleanupPlan = {
      id: "cleanup-1",
      operationId: operation.id,
      files: [],
      totalBytes: 0,
      status: "planned",
    };
    mocks.listOperations
      .mockResolvedValueOnce([operation])
      .mockResolvedValue([
        { ...operation, cleanup: { ...cleanupPlan, status: "deleting" } },
      ]);
    mocks.planCleanup.mockResolvedValue(cleanupPlan);
    mocks.cleanupHistory.mockRejectedValue(
      new Error("Cleanup stopped at a safe boundary"),
    );
    render(<OperationHistory onClose={() => {}} onComplete={() => {}} />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Preview history cleanup" }),
    );
    await screen.findByText("History cleanup preview");
    fireEvent.click(
      screen.getByLabelText(
        "I reviewed these files and accept permanently losing file undo for this operation.",
      ),
    );
    fireEvent.click(
      screen.getByRole("button", {
        name: "Delete recovery files and retire history",
      }),
    );
    await screen.findByRole("button", { name: "Review interrupted cleanup" });
    expect(
      screen.queryByRole("button", { name: "Undo file operation" }),
    ).toBeNull();
    expect(
      screen.queryByRole("button", {
        name: "Delete recovery files and retire history",
      }),
    ).toBeNull();
    expect(screen.getByRole("alert").textContent).toContain(
      "Cleanup stopped at a safe boundary",
    );
  });
  it("shows the actual recorded verification level for copied rows", async () => {
    const operation = { ...plan(), kind: "copy", status: "complete" };
    operation.rows[0].verification = "sha256-readback";
    operation.rows[0].step = "finalized";
    operation.rows[1].verification = "streamed-sha256";
    mocks.listOperations.mockResolvedValue([operation]);
    render(<OperationHistory onClose={() => {}} onComplete={() => {}} />);
    await screen.findByText("Output read-back SHA-256 verified");
    expect(screen.getByText("SHA-256 checked while copying")).toBeTruthy();
    expect(screen.getByText("finalized")).toBeTruthy();
  });
});

describe("file operation defaults and large previews", () => {
  it("snapshots persisted defaults when opened and invalidates approval after an override", async () => {
    const props = {
      kind: "copy" as const,
      ids: order.slice(0, 2),
      order,
      items,
      onClose() {},
      onComplete() {},
    };
    const view = render(
      <OperationDialog
        {...props}
        defaults={{ manifestDefault: false, verifyCopyDefault: true }}
      />,
    );
    const manifest = screen.getByLabelText(
      "Save ranking manifests (JSON and CSV)",
    ) as HTMLInputElement;
    const verify = screen.getByLabelText(
      "Verify copied bytes by reading each output back (slower)",
    ) as HTMLInputElement;
    expect(manifest.checked).toBe(false);
    expect(verify.checked).toBe(true);
    view.rerender(
      <OperationDialog
        {...props}
        defaults={{ manifestDefault: true, verifyCopyDefault: false }}
      />,
    );
    expect(manifest.checked).toBe(false);
    expect(verify.checked).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Browse" }));
    await waitFor(() =>
      expect(
        (screen.getByLabelText("Export destination") as HTMLInputElement).value,
      ).toBe("destination"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Build preview" }));
    await screen.findByText("2 planned rows");
    expect(mocks.planOperation).toHaveBeenCalledWith(
      expect.objectContaining({ manifest: false, verifyCopy: true }),
    );
    fireEvent.click(
      screen.getByLabelText(
        "I reviewed the full preview and approve these file changes.",
      ),
    );
    fireEvent.click(manifest);
    expect(
      screen.queryByLabelText(
        "I reviewed the full preview and approve these file changes.",
      ),
    ).toBeNull();
    expect(screen.getByRole("button", { name: "Build preview" })).toBeTruthy();
    expect(mocks.commitOperation).not.toHaveBeenCalled();
  });
  it("bounds 10,000-row rendering and never approves a hidden conflict", async () => {
    const operation = plan();
    operation.rows = Array.from({ length: 10000 }, (_, index) => ({
      ...operation.rows[0],
      imageId: `large-${index}`,
      rank: index + 1,
      outputNumber: index + 1,
      from: `source/large-${index}.jpg`,
      to: `target/large-${index}.jpg`,
      issue: index === 9999 ? "Target is occupied" : null,
    }));
    const ids = operation.rows.map((row) => row.imageId);
    mocks.planOperation.mockResolvedValue(operation);
    const view = render(
      <OperationDialog
        kind="rename"
        ids={ids}
        order={ids}
        items={new Map()}
        onClose={() => {}}
        onComplete={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Build preview" }));
    await screen.findByText("10000 planned rows · 1 issues to resolve");
    expect(
      view.container.querySelectorAll("[data-operation-row]").length,
    ).toBeGreaterThan(0);
    expect(
      view.container.querySelectorAll("[data-operation-row]").length,
    ).toBeLessThan(30);
    fireEvent.change(screen.getByLabelText("Search file rows"), {
      target: { value: "large-1.jpg" },
    });
    expect(screen.getByText(/Showing 1 of 10,000 rows/)).toBeTruthy();
    expect(screen.queryByText("Target is occupied")).toBeNull();
    expect(
      screen
        .getByRole("button", { name: "Rename 10000 images" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(
      screen
        .getByLabelText(
          "I reviewed the full preview and approve these file changes.",
        )
        .hasAttribute("disabled"),
    ).toBe(true);
    fireEvent.change(screen.getByLabelText("Search file rows"), {
      target: { value: "" },
    });
    fireEvent.change(screen.getByLabelText("Show rows"), {
      target: { value: "issues" },
    });
    expect(screen.getByText("Target is occupied")).toBeTruthy();
    expect(
      view.container.querySelectorAll("[data-operation-row]"),
    ).toHaveLength(1);
    expect(mocks.commitOperation).not.toHaveBeenCalled();
  });
});
