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
import {
  SelectedFolderDialog,
  folderNameError,
} from "../../src/components/SelectedFolderDialog";
import { demoReport } from "../../src/components/demo";
import type {
  ImageItem,
  MoveSelectionFolderResult,
  Operation,
  OperationProgress,
} from "../../src/native";

const mocks = vi.hoisted(() => ({
  listOperations: vi.fn(),
  subscribeOperationProgress: vi.fn(),
  getOperation: vi.fn(),
  cancelOperation: vi.fn(),
  openFolder: vi.fn(),
}));
vi.mock("../../src/native", () => ({ desktop: true, api: mocks }));
const images: ImageItem[] = demoReport().images.map((image) => ({
  ...image,
  path: `E:\\Pictures\\${image.originalName}`,
}));
const report = {
  sources: [
    {
      id: "group-source",
      path: "E:\\Pictures\\Ink",
      name: "Ink",
      recursive: false,
      count: 2,
    },
  ],
  images: images.slice(0, 2).map((image) => ({
    ...image,
    path: `E:\\Pictures\\Ink\\${image.originalName}`,
    sourceIds: ["group-source"],
  })),
  ignored: [],
};
function operation(status = "complete"): Operation {
  return {
    id: "group-move-1",
    kind: "rename",
    status,
    rows: images.slice(0, 2).map((image, index) => ({
      imageId: image.id,
      from: image.path,
      to: `E:\\Pictures\\Ink\\${image.originalName}`,
      temporary: `E:\\Pictures\\.move-${index}`,
      rank: index + 1,
      outputNumber: index + 1,
      sizeBytes: image.sizeBytes,
      issue: null,
      noop: false,
    })),
    completed: status === "complete" ? 2 : 1,
    errors: [],
    dependencies: [],
    manifestPath: null,
    order: 1,
    exfatOutputs: true,
    context: { sourceGroup: { folderName: "Ink", parentPath: "E:\\Pictures" } },
  };
}
function setup(
  overrides: Partial<Parameters<typeof SelectedFolderDialog>[0]> = {},
) {
  const props = {
    ids: images.slice(0, 2).map((image) => image.id),
    items: new Map(images.map((image) => [image.id, image])),
    eligible: true,
    move: vi
      .fn<(ids: string[], name: string) => Promise<MoveSelectionFolderResult>>()
      .mockResolvedValue({ operation: operation(), report }),
    onComplete: vi.fn().mockResolvedValue(undefined),
    onClose: vi.fn(),
    onRank: vi.fn(),
    onRecovery: vi.fn(),
    onBusyChange: vi.fn(),
    ...overrides,
  };
  const view = render(<SelectedFolderDialog {...props} />);
  return { props, ...view };
}
function approve(name = "Ink") {
  fireEvent.change(screen.getByLabelText("New subfolder name"), {
    target: { value: name },
  });
  fireEvent.click(
    screen.getByLabelText(
      "I approve moving these 2 originals into this new subfolder.",
    ),
  );
}
const create = () =>
  screen.getByRole("button", { name: "Create folder & move 2 images" });

beforeEach(() => {
  vi.resetAllMocks();
  mocks.listOperations.mockResolvedValue([]);
  mocks.subscribeOperationProgress.mockResolvedValue(() => {});
  mocks.openFolder.mockResolvedValue(undefined);
  mocks.cancelOperation.mockResolvedValue(undefined);
});
afterEach(cleanup);

describe("move selected images into a style subfolder", () => {
  it("reviews every selected file and moves only the exact frozen selection after confirmation", async () => {
    const v = setup();
    expect(create().hasAttribute("disabled")).toBe(true);
    expect(screen.getByText(images[1].path)).toBeTruthy();
    approve();
    expect(
      (screen.getByLabelText("New folder location") as HTMLInputElement).value,
    ).toBe("E:\\Pictures\\Ink");
    const button = create();
    fireEvent.click(button);
    fireEvent.click(button);
    await screen.findByRole("button", { name: "Rank this group" });
    expect(v.props.move).toHaveBeenCalledExactlyOnceWith(v.props.ids, "Ink");
    expect(v.props.onComplete).toHaveBeenCalledWith(operation(), report);
    expect(v.props.onRank).not.toHaveBeenCalled();
    expect(v.props.onClose).not.toHaveBeenCalled();
    expect(
      screen.getByText(/Undo is available only while the current app/),
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Rank this group" }));
    expect(v.props.onRank).toHaveBeenCalledExactlyOnceWith(v.props.ids);
    fireEvent.click(screen.getByRole("button", { name: "Open folder" }));
    expect(mocks.openFolder).toHaveBeenCalledWith("E:\\Pictures\\Ink");
  });
  it("refuses mixed current folders even if their imported source is the same", () => {
    const other = {
      ...images[1],
      path: `E:\\Pictures\\Other\\${images[1].originalName}`,
    };
    const v = setup({
      items: new Map([
        [images[0].id, images[0]],
        [other.id, other],
      ]),
    });
    expect(screen.getByRole("alert").textContent).toContain(
      "different current folders",
    );
    expect(create().hasAttribute("disabled")).toBe(true);
    expect(v.props.move).not.toHaveBeenCalled();
  });
  it("clears approval when the folder name changes and shows Windows name errors", () => {
    const v = setup();
    approve();
    expect(create().hasAttribute("disabled")).toBe(false);
    fireEvent.change(screen.getByLabelText("New subfolder name"), {
      target: { value: "../Ink" },
    });
    expect(create().hasAttribute("disabled")).toBe(true);
    expect((screen.getByRole("checkbox") as HTMLInputElement).checked).toBe(
      false,
    );
    expect(screen.getByText(/without slashes/)).toBeTruthy();
    expect(v.props.move).not.toHaveBeenCalled();
  });
  it("keeps the dialog open and ranking disabled until the returned catalog is saved", async () => {
    let finish!: () => void;
    const saving = new Promise<void>((resolve) => {
      finish = resolve;
    });
    const v = setup({ onComplete: vi.fn(() => saving) });
    approve();
    fireEvent.click(create());
    const rank = await screen.findByRole("button", { name: "Rank this group" });
    expect(rank.hasAttribute("disabled")).toBe(true);
    expect(
      screen
        .getByRole("button", { name: "Close dialog" })
        .hasAttribute("disabled"),
    ).toBe(true);
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(v.props.onClose).not.toHaveBeenCalled();
    await act(async () => finish());
    await waitFor(() => expect(rank.hasAttribute("disabled")).toBe(false));
  });
  it("offers recovery after an interrupted move instead of blindly moving the group again", async () => {
    const partial = operation("interrupted");
    mocks.listOperations.mockResolvedValueOnce([]).mockResolvedValue([partial]);
    const v = setup({
      move: vi.fn().mockRejectedValue(new Error("Drive disconnected")),
    });
    approve();
    fireEvent.click(create());
    const recovery = await screen.findByRole("button", {
      name: "Open recovery history",
    });
    expect(screen.getByRole("alert").textContent).toContain(
      "Drive disconnected",
    );
    expect(screen.queryByRole("button", { name: /Create folder/ })).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Rank this group" }),
    ).toBeNull();
    expect(v.props.onComplete).toHaveBeenCalledWith(partial);
    fireEvent.click(recovery);
    expect(v.props.onRecovery).toHaveBeenCalledOnce();
  });
  it("keeps ranking disabled after a successful move whose catalog could not be saved, and retries only saving", async () => {
    const complete = operation();
    mocks.listOperations
      .mockResolvedValueOnce([])
      .mockResolvedValue([complete]);
    const onComplete = vi
      .fn()
      .mockRejectedValue(new Error("Could not save folder changes"));
    const v = setup({ onComplete });
    approve();
    fireEvent.click(create());
    const rank = await screen.findByRole("button", { name: "Rank this group" });
    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toContain(
        "Could not save folder changes",
      ),
    );
    expect(rank.hasAttribute("disabled")).toBe(true);
    onComplete.mockResolvedValue(undefined);
    fireEvent.click(screen.getByRole("button", { name: "Retry saving group" }));
    await waitFor(() => expect(rank.hasAttribute("disabled")).toBe(false));
    expect(v.props.move).toHaveBeenCalledOnce();
    expect(onComplete).toHaveBeenLastCalledWith(complete);
  });
  it("binds progress to the new group's receipt and permits a safe-boundary stop", async () => {
    let progress!: (event: OperationProgress) => void;
    mocks.subscribeOperationProgress.mockImplementation(async (handler) => {
      progress = handler;
      return () => {};
    });
    let finish!: (result: MoveSelectionFolderResult) => void;
    const moving = new Promise<MoveSelectionFolderResult>((resolve) => {
      finish = resolve;
    });
    setup({ move: vi.fn(() => moving) });
    approve();
    fireEvent.click(create());
    await waitFor(() => expect(progress).toBeTypeOf("function"));
    const receipt = { ...operation("running"), sequence: 1 };
    await act(async () =>
      progress({
        id: receipt.id,
        sequence: 1,
        status: "running",
        completed: 1,
        total: 2,
        rows: [],
        errors: [],
        historyRetired: false,
        snapshot: receipt,
      }),
    );
    expect(screen.getByText(/1 steps completed/)).toBeTruthy();
    fireEvent.click(
      screen.getByRole("button", { name: "Stop at safe boundary" }),
    );
    expect(mocks.cancelOperation).toHaveBeenCalledOnce();
    await act(async () => finish({ operation: operation(), report }));
  });
});

describe("Windows subfolder names", () => {
  it.each([
    "",
    " ",
    ".",
    "..",
    "Ink/one",
    "Ink\\one",
    "Ink?",
    "Ink.",
    "Ink ",
    "CON",
    "nul.jpg",
    "LPT1",
    "COM¹.png",
    "CONIN$",
    "conout$.png",
    "CON .png",
    "LPT2  .txt",
    "Ink\u007f",
    "Ink\u0085",
  ])("rejects %j before asking native code to move anything", (name) =>
    expect(folderNameError(name)).not.toBe(""),
  );
  it("accepts descriptive Unicode names and ordinary dots within a name", () => {
    expect(folderNameError("Watercolors 画 · v2.1")).toBe("");
  });
  it("matches the native limit of 200 Windows text units, including Unicode pairs", () => {
    expect(folderNameError("a".repeat(200))).toBe("");
    expect(folderNameError("a".repeat(201))).not.toBe("");
    expect(folderNameError("🌿".repeat(100))).toBe("");
    expect(folderNameError("🌿".repeat(101))).not.toBe("");
  });
});
