// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import {
  OperationDialog,
  OperationHistory,
} from "../../src/components/OperationDialog";
import { demoReport } from "../../src/components/demo";
import type { Operation, PlanRequest } from "../../src/native";

const mocks = vi.hoisted(() => ({
  planOperation: vi.fn(),
  chooseDestination: vi.fn(),
  listOperations: vi.fn(),
  recoverOperation: vi.fn(),
  undoOperation: vi.fn(),
  subscribeOperationProgress: vi.fn(),
}));
vi.mock("../../src/native", () => ({ desktop: true, api: mocks }));

const images = demoReport().images;
const items = new Map(images.map((image) => [image.id, image]));
const order = images.map((image) => image.id);

function operation(patch: Partial<Operation> = {}): Operation {
  return {
    id: "exfat-rename-1",
    kind: "rename",
    status: "planned",
    rows: order.slice(0, 2).map((imageId, index) => ({
      imageId,
      from: `E:\\Pictures\\photo-${index}.jpg`,
      to: `E:\\Pictures\\00${index + 1}.jpg`,
      temporary: `E:\\Pictures\\.temporary-${index}`,
      rank: index + 1,
      outputNumber: index + 1,
      sizeBytes: 1024,
      issue: null,
      noop: false,
    })),
    errors: [],
    completed: 0,
    manifestPath: null,
    order: 1,
    dependencies: [],
    exfatOutputs: true,
    ...patch,
  };
}

function showDialog(kind: "rename" | "copy") {
  return render(
    <OperationDialog
      kind={kind}
      ids={order.slice(0, 2)}
      order={order}
      items={items}
      onClose={() => {}}
      onComplete={() => {}}
    />,
  );
}

beforeEach(() => {
  vi.resetAllMocks();
  mocks.planOperation.mockImplementation(async (request: PlanRequest) =>
    operation({ kind: request.kind }),
  );
  mocks.chooseDestination.mockResolvedValue("E:\\Exports");
  mocks.listOperations.mockResolvedValue([]);
  mocks.subscribeOperationProgress.mockResolvedValue(() => {});
});
afterEach(cleanup);

describe("exFAT original rename notices", () => {
  it("shows original rename protection after a destination-free preview, without export conflict advice", async () => {
    showDialog("rename");
    fireEvent.click(screen.getByRole("button", { name: "Build preview" }));
    await screen.findByText("2 planned rows");
    expect(mocks.planOperation).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "rename", destination: null }),
    );
    const notice = screen.getByText(/Renaming changes filenames only/);
    expect(notice.textContent).toContain("image contents stay the same");
    expect(notice.textContent).toContain("protected against edits");
    expect(notice.textContent).toContain("current open app and profile");
    expect(notice.textContent).toContain("Closing releases those locks");
    expect(notice.textContent).toContain("after reopening");
    expect(notice.textContent).not.toMatch(/export|suffix|skip|Replace/i);
    expect(screen.queryByLabelText("Export destination")).toBeNull();
    expect(
      screen
        .getByRole("button", { name: "Rename 2 images" })
        .hasAttribute("disabled"),
    ).toBe(true);
  });

  it("leaves NTFS rename previews free of the exFAT lock notice", async () => {
    mocks.planOperation.mockResolvedValue(operation({ exfatOutputs: false }));
    showDialog("rename");
    fireEvent.click(screen.getByRole("button", { name: "Build preview" }));
    await screen.findByText("2 planned rows");
    expect(screen.queryByText(/current open app and profile/)).toBeNull();
  });

  it("describes completed ordinary rename history without implying exported copies", async () => {
    mocks.listOperations.mockResolvedValue([
      operation({ status: "complete", completed: 4 }),
    ]);
    render(<OperationHistory onClose={() => {}} onComplete={() => {}} />);
    await screen.findByText("Rename · 2 files");
    expect(screen.getByText(/Renaming changes filenames only/)).toBeTruthy();
    expect(screen.queryByText(/exFAT exports/)).toBeNull();
    expect(
      screen.getByRole("button", { name: "Undo file operation" }),
    ).toBeTruthy();
  });

  it("confirms keeping interrupted original names and locations before using the existing abandon command", async () => {
    const pending = operation({ status: "needs-recovery", completed: 1 });
    const dismissed = { ...pending, status: "abandoned", historyRetired: true };
    const complete = vi.fn();
    mocks.listOperations.mockResolvedValue([pending]);
    mocks.recoverOperation.mockResolvedValue(dismissed);
    render(<OperationHistory onClose={() => {}} onComplete={complete} />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Keep files where they are" }),
    );
    const confirmation = screen.getByText(
      /Keep every image with its current name and location/,
    );
    expect(confirmation.textContent).toContain("ends Undo and Resume");
    expect(confirmation.textContent).toContain(
      "No files will be deleted, renamed or moved",
    );
    expect(screen.queryByText(/unfinished export/)).toBeNull();
    expect(mocks.recoverOperation).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(mocks.recoverOperation).not.toHaveBeenCalled();
    fireEvent.click(
      screen.getByRole("button", { name: "Keep files where they are" }),
    );
    mocks.listOperations.mockResolvedValue([dismissed]);
    fireEvent.click(
      screen.getByRole("button", { name: "Confirm keep files and dismiss" }),
    );
    await screen.findByText("Files kept · dismissed");
    expect(mocks.recoverOperation).toHaveBeenCalledExactlyOnceWith(
      pending.id,
      "abandon",
    );
    expect(complete).toHaveBeenCalledExactlyOnceWith(dismissed);
    expect(
      screen.getByText(
        /Renamed and partial files were kept with their current names and locations/,
      ),
    ).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: "Resume operation" }),
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Undo file operation" }),
    ).toBeNull();
    expect(mocks.undoOperation).not.toHaveBeenCalled();
  });

  it("keeps interrupted rename recovery available if native dismissal fails", async () => {
    mocks.listOperations.mockResolvedValue([
      operation({ status: "needs-recovery" }),
    ]);
    mocks.recoverOperation.mockRejectedValue(
      new Error("Cannot preserve dismissal"),
    );
    const complete = vi.fn();
    render(<OperationHistory onClose={() => {}} onComplete={complete} />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Keep files where they are" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Confirm keep files and dismiss" }),
    );
    await screen.findByText("Error: Cannot preserve dismissal");
    expect(screen.queryByText("Files kept · dismissed")).toBeNull();
    expect(
      screen.getByRole("button", { name: "Resume operation" }),
    ).toBeTruthy();
    expect(complete).not.toHaveBeenCalled();
  });

  it("retains source-group move protection and keep-in-place wording", async () => {
    mocks.listOperations.mockResolvedValue([
      operation({
        status: "needs-recovery",
        context: {
          sourceGroup: { parentPath: "E:\\Pictures", folderName: "Ink" },
        },
      }),
    ]);
    render(<OperationHistory onClose={() => {}} onComplete={() => {}} />);
    await screen.findByText("Move to subfolder · 2 files");
    expect(
      screen.getByText(/These moved images remain protected/),
    ).toBeTruthy();
    fireEvent.click(
      screen.getByRole("button", { name: "Keep files where they are" }),
    );
    expect(
      screen.getByText(/Keep every image in its current location/),
    ).toBeTruthy();
    expect(screen.queryByText(/unfinished export/)).toBeNull();
  });

  it("preserves exFAT copy conflict advice and export dismissal wording", async () => {
    showDialog("copy");
    fireEvent.click(screen.getByRole("button", { name: "Browse" }));
    await waitFor(() =>
      expect(
        (screen.getByLabelText("Export destination") as HTMLInputElement).value,
      ).toBe("E:\\Exports"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Build preview" }));
    await screen.findByText(/exFAT exports stay protected/);
    expect(
      screen.getByText(
        /Choose a numbered suffix or skip existing files; Replace is unavailable/,
      ),
    ).toBeTruthy();
    expect(
      screen
        .getByRole("option", { name: "Replace, keeping a recovery backup" })
        .hasAttribute("disabled"),
    ).toBe(true);
    cleanup();
    mocks.listOperations.mockResolvedValue([
      operation({ kind: "copy", status: "needs-recovery" }),
    ]);
    render(<OperationHistory onClose={() => {}} onComplete={() => {}} />);
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Keep files and dismiss export",
      }),
    );
    expect(
      screen.getByText(/Keep all completed and partial export files/),
    ).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: "Keep files where they are" }),
    ).toBeNull();
  });
});
