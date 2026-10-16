import { useEffect, useRef, useState } from "react";
import { ArrowRight, FolderOpen, LoaderCircle } from "lucide-react";
import {
  api,
  desktop,
  type ImageItem,
  type Operation,
  type MoveSelectionFolderResult,
  type ScanReport,
} from "../native";
import { Modal } from "./Modal";
import { basename, bytes } from "./OperationDialog";
import { operationProgressReceiver } from "./operationProgress";

const parentPath = (path: string) => path.replace(/[\\/][^\\/]*$/, "");
const comparablePath = (path: string) =>
  path.replaceAll("/", "\\").toLocaleLowerCase();

export function folderNameError(name: string): string {
  if (!name.trim()) return "Enter a name for the new subfolder.";
  if (/[<>:"/\\|?*\u0000-\u001f\u007f-\u009f]/.test(name))
    return 'Use a folder name without slashes or these characters: < > : " | ? *.';
  if (/[. ]$/.test(name))
    return "A folder name cannot end with a dot or a space.";
  const base = name.split(".")[0].replace(/ +$/, "");
  if (
    /^(con|prn|aux|nul|conin\$|conout\$|com[1-9¹²³]|lpt[1-9¹²³])$/i.test(base)
  )
    return "That name is reserved by Windows. Choose another name.";
  if (name.length > 200)
    return "This folder name is too long. Use a shorter name.";
  return "";
}

export function SelectedFolderDialog({
  ids,
  items,
  eligible,
  move,
  onClose,
  onComplete,
  onRank,
  onRecovery,
  onBusyChange,
}: {
  ids: string[];
  items: Map<string, ImageItem>;
  eligible: boolean;
  move: (
    ids: string[],
    folderName: string,
  ) => Promise<MoveSelectionFolderResult>;
  onClose: () => void;
  onComplete: (
    operation: Operation,
    report?: ScanReport,
  ) => Promise<void> | void;
  onRank: (ids: string[]) => void;
  onRecovery: () => void;
  onBusyChange: (busy: boolean) => void;
}) {
  // The reviewed group includes every selected image, even one hidden by a filter.
  const [selectedIds] = useState(() => [...ids]);
  const [selectedItems] = useState(() =>
    selectedIds.map((id) => items.get(id)),
  );
  const firstParent = selectedItems[0] && parentPath(selectedItems[0].path);
  const commonParent =
    firstParent &&
    selectedItems.every(
      (item) =>
        item &&
        (!item.availability || item.availability === "ready") &&
        comparablePath(parentPath(item.path)) === comparablePath(firstParent),
    )
      ? firstParent
      : "";
  const [folderName, setFolderName] = useState("Style group"),
    [confirmed, setConfirmed] = useState(false),
    [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [catalogSaved, setCatalogSaved] = useState(false),
    [phase, setPhase] = useState<"moving" | "saving">("moving"),
    [operation, setOperation] = useState<Operation | null>(null);
  const inFlight = useRef(false);
  const destination = commonParent ? `${commonParent}\\${folderName}` : "";
  const nameError = folderNameError(folderName);
  const totalBytes = selectedItems.reduce(
    (total, item) => total + (item?.sizeBytes || 0),
    0,
  );
  const finished = operation?.status === "complete";
  useEffect(() => {
    onBusyChange(busy);
    return () => onBusyChange(false);
  }, [busy, onBusyChange]);

  const moveNow = async () => {
    if (
      inFlight.current ||
      operation ||
      !desktop ||
      !eligible ||
      !selectedIds.length ||
      !commonParent ||
      nameError ||
      !confirmed
    )
      return;
    inFlight.current = true;
    setBusy(true);
    setPhase("moving");
    setError("");
    let detach: (() => void) | undefined;
    let previous: Set<string> | undefined;
    let currentId: string | undefined;
    const matches = (candidate: Operation) =>
      candidate.rows.length === selectedIds.length &&
      new Set(candidate.rows.map((row) => row.imageId)).size ===
        selectedIds.length &&
      candidate.rows.every(
        (row) =>
          selectedIds.includes(row.imageId) &&
          comparablePath(parentPath(row.to)) === comparablePath(destination),
      );
    const progress = operationProgressReceiver(
      null,
      setOperation,
      api.getOperation,
      setError,
    );
    try {
      previous = new Set((await api.listOperations()).map((value) => value.id));
      detach = await api.subscribeOperationProgress((event) => {
        if (
          !currentId &&
          event.snapshot &&
          !previous!.has(event.id) &&
          matches(event.snapshot)
        )
          currentId = event.id;
        if (event.id === currentId) progress.accept(event);
      });
      const result = await move(selectedIds, folderName);
      const receipt = result.operation;
      progress.stop();
      setOperation(receipt);
      setPhase("saving");
      await onComplete(receipt, result.report);
      setCatalogSaved(true);
      if (receipt.status !== "complete")
        setError(
          receipt.errors.join("\n") ||
            `Move ${receipt.status}. Open file operation history to inspect recovery.`,
        );
    } catch (reason) {
      progress.stop();
      setError(String(reason));
      if (previous) {
        try {
          const latest = (await api.listOperations()).find(
            (value) => !previous!.has(value.id) && matches(value),
          );
          if (latest) {
            setOperation(latest);
            setPhase("saving");
            await onComplete(latest);
            setCatalogSaved(true);
          }
        } catch {
          // Keep the execution/save error visible; recovery remains in the native journal.
        }
      }
    } finally {
      progress.stop();
      detach?.();
      inFlight.current = false;
      setBusy(false);
    }
  };
  const savedFolder = operation?.rows[0]
    ? parentPath(operation.rows[0].to)
    : destination;
  const retrySave = async () => {
    if (inFlight.current || !operation) return;
    inFlight.current = true;
    setBusy(true);
    setPhase("saving");
    setError("");
    try {
      await onComplete(operation);
      setCatalogSaved(true);
    } catch (reason) {
      setError(String(reason));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  return (
    <Modal
      title="Move selection to a subfolder"
      wide
      onClose={onClose}
      closeDisabled={busy}
      footer={
        <>
          <span className="footer-note">
            {selectedIds.length} images · {bytes(totalBytes)}
          </span>
          <button disabled={busy} onClick={onClose}>
            {operation ? "Close" : "Cancel"}
          </button>
          {busy && phase === "moving" && (
            <button
              onClick={() =>
                api
                  .cancelOperation()
                  .catch((reason) => setError(String(reason)))
              }
            >
              Stop at safe boundary
            </button>
          )}
          {!operation && (
            <button
              className="primary"
              onClick={() => void moveNow()}
              disabled={
                busy ||
                !desktop ||
                !eligible ||
                !selectedIds.length ||
                !commonParent ||
                !!nameError ||
                !confirmed
              }
            >
              {busy ? (
                <>
                  <LoaderCircle size={16} className="spin" /> Moving images…
                </>
              ) : (
                `Create folder & move ${selectedIds.length} images`
              )}
            </button>
          )}
          {operation && !finished && !busy && (
            <button className="primary" onClick={onRecovery}>
              Open recovery history
            </button>
          )}
          {finished && (
            <>
              {!catalogSaved && (
                <button disabled={busy} onClick={() => void retrySave()}>
                  Retry saving group
                </button>
              )}
              <button
                className="primary"
                disabled={busy || !eligible || !catalogSaved}
                onClick={() => onRank(selectedIds)}
              >
                Rank this group <ArrowRight size={16} />
              </button>
            </>
          )}
        </>
      }
    >
      <p className="muted">
        Create a new subfolder inside the images’ current folder and move the
        selected originals into it. Filenames and image contents stay the same.
        Any selected images hidden by search or folder filters are included.
      </p>
      {!desktop && (
        <p className="notice">
          Moving real files requires the Windows desktop app.
        </p>
      )}
      {!commonParent && (
        <p className="error" role="alert">
          Select available images from one folder to create a separate group.
          Images from different current folders cannot be moved together here.
        </p>
      )}
      <div className="form-grid">
        <label className="full">
          New subfolder name
          <input
            aria-label="New subfolder name"
            value={folderName}
            disabled={busy || !!operation}
            aria-invalid={!!nameError}
            onChange={(event) => {
              setFolderName(event.target.value);
              setConfirmed(false);
              setError("");
            }}
          />
          {nameError && <small className="error">{nameError}</small>}
        </label>
        <label className="full">
          New folder location
          <input
            aria-label="New folder location"
            value={destination}
            readOnly
            title={destination}
          />
          <small>
            A new folder is required. Existing folders are never reused.
          </small>
        </label>
      </div>
      <details>
        <summary>Review all {selectedIds.length} selected images</summary>
        <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th>Image</th>
                <th>Current location</th>
              </tr>
            </thead>
            <tbody>
              {selectedItems.map((item, index) => (
                <tr key={selectedIds[index]}>
                  <td>{item ? basename(item.path) : "Unavailable image"}</td>
                  <td className="break">{item?.path || selectedIds[index]}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </details>
      {!operation && (
        <label className="check-label confirm">
          <input
            type="checkbox"
            checked={confirmed}
            disabled={busy || !commonParent || !!nameError}
            onChange={(event) => setConfirmed(event.target.checked)}
          />
          I approve moving these {selectedIds.length} originals into this new
          subfolder.
        </label>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {busy && operation && (
        <p className="notice" role="status">
          <LoaderCircle className="spin" size={17} />
          {phase === "saving"
            ? "Saving folder changes…"
            : `Moving images · ${operation.completed} steps completed. This window stays open until a safe boundary.`}
        </p>
      )}
      {finished && (
        <>
          <p className="notice" role="status">
            Moved {selectedIds.length} images. Your current ranking is preserved
            until you choose to rank this group.
          </p>
          <p className="muted break">Saved folder: {savedFolder}</p>
          {operation.exfatOutputs && (
            <p className="notice">
              On this drive, Undo is available only while the current app and
              profile remain open. Closing releases the moved files’ protection
              against edits and keeps the images in their new folder.
            </p>
          )}
          <button
            disabled={busy}
            onClick={() =>
              api
                .openFolder(savedFolder)
                .catch((reason) => setError(String(reason)))
            }
          >
            <FolderOpen size={16} /> Open folder
          </button>
        </>
      )}
    </Modal>
  );
}
