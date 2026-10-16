import { useState, useMemo, useEffect, useRef } from "react";
import {
  FolderOpen,
  ArrowRight,
  AlertTriangle,
  CheckCircle2,
  LoaderCircle,
} from "lucide-react";
import {
  api,
  desktop,
  type ImageItem,
  type Operation,
  type CleanupPlan,
  type PlanRequest,
} from "../native";
import { Modal } from "./Modal";
import { OperationRows } from "./OperationRows";
import { operationProgressReceiver } from "./operationProgress";

export const basename = (path: string) => path.split(/[\\/]/).at(-1) || path;
export const bytes = (value: number) =>
  value < 1024
    ? `${value} B`
    : value < 1048576
      ? `${(value / 1024).toFixed(1)} KB`
      : `${(value / 1048576).toFixed(1)} MB`;
function ExfatNotice({
  dismissed = false,
  sourceChange = false,
  movedGroup = false,
}: {
  dismissed?: boolean;
  sourceChange?: boolean;
  movedGroup?: boolean;
}) {
  return (
    <p className="notice">
      {sourceChange
        ? movedGroup
          ? dismissed
            ? "Moved and partial files were kept in their current locations. This move was dismissed; Undo and Resume are no longer available."
            : "These moved images remain protected against edits while Image Ranker holds them open. Undo is available only in the current open app and profile. Closing releases those locks and keeps the files in place; old move records cannot be used to move files after reopening."
          : dismissed
            ? "Renamed and partial files were kept with their current names and locations. This rename was dismissed; Undo and Resume are no longer available."
            : "Renaming changes filenames only; image contents stay the same. These originals remain protected against edits while Image Ranker holds them open. Undo is available only in the current open app and profile. Closing releases those locks and keeps current names; old rename records cannot be used to rename files after reopening."
        : dismissed
          ? "Completed and partial files were kept. This export was dismissed and Undo is no longer available. You can start a new export."
          : "exFAT exports stay protected against edits while Image Ranker holds them open. Undo is available only in the current open app and profile; closing releases those locks. Old export records cannot be used to delete files after reopening. Choose a numbered suffix or skip existing files; Replace is unavailable."}
    </p>
  );
}
export function OperationDialog({
  kind,
  ids,
  order,
  items,
  onClose,
  onComplete,
  onRecovery,
  scope = "picks",
  defaults,
  onBusyChange,
}: {
  kind: "rename" | "copy";
  ids: string[];
  order: string[];
  items: Map<string, ImageItem>;
  onClose: () => void;
  onComplete: (operation: Operation) => Promise<void> | void;
  onRecovery?: () => void;
  scope?: "picks" | "all";
  defaults?: { manifestDefault?: boolean; verifyCopyDefault?: boolean };
  onBusyChange?: (busy: boolean) => void;
}) {
  const [request, setRequest] = useState<PlanRequest>(() => ({
    kind,
    imageIds: ids,
    ranks: (() => {
      const ranks = new Map(order.map((id, index) => [id, index + 1]));
      return ids.map((id) => ranks.get(id)!);
    })(),
    reviewCount: order.length,
    naming: "numeric",
    template: "{rank}.{ext}",
    start: 1,
    padding: Math.max(3, String(order.length).length),
    destination: null,
    conflict: "suffix",
    manifest: defaults?.manifestDefault ?? true,
    verifyCopy: defaults?.verifyCopyDefault ?? false,
  }));
  const [plan, setPlan] = useState<Operation | null>(null),
    [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [confirmed, setConfirmed] = useState(false),
    [exfatDestination, setExfatDestination] = useState<string | null>(null);
  useEffect(() => {
    onBusyChange?.(busy);
    return () => onBusyChange?.(false);
  }, [busy, onBusyChange]);
  const change = (patch: Partial<PlanRequest>) => {
    if (patch.destination !== undefined) setExfatDestination(null);
    setRequest((v) => ({ ...v, ...patch }));
    setPlan(null);
    setConfirmed(false);
    setError("");
  };
  const totalBytes = useMemo(
    () => ids.reduce((sum, id) => sum + (items.get(id)?.sizeBytes || 0), 0),
    [ids, items],
  );
  const planNow = async () => {
    if (!desktop) {
      setError(
        "File operations are available in the desktop app. Browser demo images are generated examples.",
      );
      return;
    }
    setBusy(true);
    setError("");
    setConfirmed(false);
    setPlan(null);
    try {
      const preview = await api.planOperation(request);
      setPlan(preview);
      setExfatDestination(preview.exfatOutputs ? request.destination : null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const commit = async () => {
    if (!plan || busy) return;
    setBusy(true);
    setError("");
    let detach: (() => void) | undefined;
    const progress = operationProgressReceiver(
      plan,
      setPlan,
      api.getOperation,
      setError,
    );
    try {
      detach = await api.subscribeOperationProgress((operation) => {
        if (operation.id === plan.id) progress.accept(operation);
      });
      const operation = await api.commitOperation(plan.id);
      setPlan(operation);
      await onComplete(operation);
      if (operation.status !== "complete")
        setError(
          operation.errors.join("\n") ||
            `Operation ${operation.status}. Open operation history to inspect recovery.`,
        );
    } catch (e) {
      setError(String(e));
      try {
        const latest = (await api.listOperations()).find(
          (operation) => operation.id === plan.id,
        );
        if (latest) {
          setPlan(latest);
          await onComplete(latest);
        }
      } catch {
        /* Keep the original execution error visible if reconciliation also fails. */
      }
    } finally {
      progress.stop();
      detach?.();
      setBusy(false);
    }
  };
  const finished = !!plan && plan.status === "complete",
    started = !!plan && plan.status !== "planned",
    conflicts =
      plan?.rows.filter((row) => row.issue && !/^skip/i.test(row.issue))
        .length || 0;
  return (
    <Modal
      title={
        kind === "rename"
          ? "Rename ranked images"
          : scope === "all"
            ? "Create ranked copies"
            : "Export final picks"
      }
      onClose={() => {
        if (!busy) onClose();
      }}
      wide
      closeDisabled={!!busy}
      footer={
        <>
          <span className="footer-note">
            {ids.length} images · {bytes(totalBytes)} · originals{" "}
            {kind === "copy" ? "stay in place" : "keep their contents"}
          </span>
          <button onClick={onClose} disabled={busy}>
            {started ? "Close" : "Cancel"}
          </button>
          {plan && !started && (
            <button disabled={busy} onClick={planNow}>
              Refresh preview
            </button>
          )}
          {busy && started && (
            <button
              onClick={() =>
                api.cancelOperation().catch((e) => setError(String(e)))
              }
            >
              Stop at safe boundary
            </button>
          )}
          {!started && (
            <button
              className="primary"
              onClick={plan ? commit : planNow}
              disabled={
                busy ||
                !ids.length ||
                (kind === "copy" && !request.destination) ||
                !!conflicts ||
                (!!plan && !confirmed)
              }
            >
              {busy ? (
                <>
                  <LoaderCircle className="spin" size={16} />{" "}
                  {plan ? "Applying changes…" : "Checking files…"}
                </>
              ) : plan ? (
                `${kind === "rename" ? "Rename" : "Copy"} ${ids.length} images`
              ) : (
                "Build preview"
              )}
            </button>
          )}
          {started &&
            !finished &&
            plan?.status !== "abandoned" &&
            !busy &&
            onRecovery && (
              <button className="primary" onClick={onRecovery}>
                Open recovery history
              </button>
            )}
        </>
      }
    >
      <p className="muted">
        {kind === "rename"
          ? "Rename files in their existing folders. Review every destination before applying changes."
          : scope === "all"
            ? "Create a ranked copy of every image in a chosen folder. Originals keep their current names and locations."
            : "Copy your final picks to a chosen folder. Ranked names preserve positions from the full ranking."}
      </p>
      {!desktop && (
        <p className="notice">
          Browser demo · Real file operations require the desktop app.
        </p>
      )}
      <div className="form-grid">
        <label>
          Naming
          <select
            disabled={busy || started}
            value={request.naming}
            onChange={(e) =>
              change({ naming: e.target.value as PlanRequest["naming"] })
            }
          >
            <option value="numeric">Rank — 001.jpg</option>
            <option value="prefix">Rank + original — 001__photo.jpg</option>
            <option value="custom">Custom template</option>
            {kind === "copy" && (
              <>
                <option value="original">Original filenames</option>
                <option value="current">Current filenames</option>
              </>
            )}
          </select>
        </label>
        <label>
          Starting number
          <input
            type="number"
            min="0"
            max="999999999"
            value={request.start}
            disabled={
              busy ||
              started ||
              ["original", "current"].includes(request.naming)
            }
            onChange={(e) =>
              change({
                start: Math.max(0, Math.trunc(Number(e.target.value) || 0)),
              })
            }
          />
        </label>
        <label>
          Minimum digits
          <input
            type="number"
            min="1"
            max="12"
            value={request.padding}
            disabled={
              busy ||
              started ||
              ["original", "current"].includes(request.naming)
            }
            onChange={(e) =>
              change({
                padding: Math.max(1, Math.min(12, Number(e.target.value) || 1)),
              })
            }
          />
        </label>
        {request.naming === "custom" && (
          <label>
            Template
            <input
              value={request.template}
              disabled={busy || started}
              onChange={(e) => change({ template: e.target.value })}
            />
            <small>
              Tokens: {"{rank}"}, {"{original}"}, {"{stem}"}, {"{ext}"}.
              Preserve the source extension.
            </small>
          </label>
        )}
        {kind === "copy" && (
          <>
            <label className="full">
              Destination
              <div className="input-action">
                <input
                  aria-label="Export destination"
                  value={request.destination || ""}
                  readOnly
                  placeholder="Choose a destination folder"
                />
                <button
                  disabled={!desktop || busy || started}
                  onClick={async () => {
                    try {
                      const destination = await api.chooseDestination();
                      if (destination) change({ destination });
                    } catch (e) {
                      setError(String(e));
                    }
                  }}
                >
                  <FolderOpen size={17} /> Browse
                </button>
              </div>
            </label>
            <label>
              If a file already exists
              <select
                value={request.conflict}
                disabled={busy || started}
                onChange={(e) =>
                  change({
                    conflict: e.target.value as PlanRequest["conflict"],
                  })
                }
              >
                <option value="suffix">Add a numbered suffix</option>
                <option value="skip">Skip existing files</option>
                <option value="overwrite" disabled={!!exfatDestination}>
                  Replace, keeping a recovery backup
                </option>
              </select>
            </label>
          </>
        )}
      </div>
      {(plan?.exfatOutputs || exfatDestination) && (
        <ExfatNotice
          dismissed={plan?.status === "abandoned"}
          sourceChange={kind === "rename"}
        />
      )}
      <label className="check-label">
        <input
          type="checkbox"
          checked={request.manifest}
          disabled={busy || started}
          onChange={(e) => change({ manifest: e.target.checked })}
        />{" "}
        Save ranking manifests (JSON and CSV)
      </label>
      {kind === "copy" && (
        <label className="check-label">
          <input
            type="checkbox"
            checked={request.verifyCopy}
            disabled={busy || started}
            onChange={(e) => change({ verifyCopy: e.target.checked })}
          />{" "}
          Verify copied bytes by reading each output back (slower)
        </label>
      )}
      {error && (
        <p role="alert" className="error">
          <AlertTriangle size={17} />
          {error}
        </p>
      )}
      {plan && (
        <>
          <div className="preview-summary">
            {finished ? (
              <span className="success">
                <CheckCircle2 size={18} /> Operation completed
              </span>
            ) : started ? (
              <strong>
                {["files-committed", "artifact-repair-only"].includes(
                  plan.status,
                )
                  ? "Files saved · manifest needs attention"
                  : plan.status === "abandoned"
                    ? "Files kept · dismissed"
                    : plan.status}{" "}
                · {plan.completed} steps completed
              </strong>
            ) : (
              <strong>
                {plan.rows.length} planned rows
                {conflicts ? ` · ${conflicts} issues to resolve` : ""}
              </strong>
            )}
            <span className="muted">
              {kind === "rename"
                ? "Global rank across all source folders"
                : "Global positions preserved for this subset"}
            </span>
          </div>
          <OperationRows operation={plan} />
          {!started && (
            <label className="check-label confirm">
              <input
                type="checkbox"
                checked={confirmed}
                disabled={busy || !!conflicts}
                onChange={(e) => setConfirmed(e.target.checked)}
              />
              {request.conflict === "overwrite" && kind === "copy"
                ? "I reviewed the full preview and approve replacing existing destination files with recovery backups."
                : "I reviewed the full preview and approve these file changes."}
            </label>
          )}
          {finished && plan.manifestPath && (
            <p className="muted break">Manifest: {plan.manifestPath}</p>
          )}
          {finished && request.destination && (
            <button
              onClick={() =>
                api
                  .openFolder(request.destination!)
                  .catch((e) => setError(String(e)))
              }
            >
              <FolderOpen size={17} /> Open destination
            </button>
          )}
        </>
      )}
      {!plan && (
        <div className="empty-preview">
          <ArrowRight size={26} />
          <span>
            Build a preview to inspect all names, destinations and conflicts.
          </span>
        </div>
      )}
    </Modal>
  );
}

export function OperationHistory({
  onClose,
  onComplete,
  recoveryOnly = false,
  onBusyChange,
}: {
  onClose: () => void;
  onComplete: (operation: Operation) => Promise<void> | void;
  recoveryOnly?: boolean;
  onBusyChange?: (busy: boolean) => void;
}) {
  const [operations, setOperations] = useState<Operation[]>([]),
    [error, setError] = useState(""),
    [busy, setBusy] = useState(""),
    [loading, setLoading] = useState(desktop),
    [busyKind, setBusyKind] = useState<"operation" | "preview" | "cleanup">(
      "operation",
    ),
    [cleanupPlan, setCleanupPlan] = useState<CleanupPlan | null>(null),
    [cleanupApproved, setCleanupApproved] = useState(false),
    [confirm, setConfirm] = useState<{
      id: string;
      action: "undo" | "resume" | "rollback" | "abandon";
    } | null>(null);
  const operationsRef = useRef(operations);
  operationsRef.current = operations;
  const reload = () => {
    if (desktop) {
      setLoading(true);
      setError("");
      api
        .listOperations()
        .then(setOperations)
        .catch((e) => setError(String(e)))
        .finally(() => setLoading(false));
    }
  };
  useEffect(() => {
    onBusyChange?.(!!busy);
    return () => onBusyChange?.(false);
  }, [busy, onBusyChange]);
  useEffect(reload, []);
  useEffect(() => {
    if (!desktop) return;
    const receivers = new Map<
      string,
      ReturnType<typeof operationProgressReceiver>
    >();
    let active = true,
      detach: (() => void) | undefined;
    api
      .subscribeOperationProgress((event) => {
        if (!active) return;
        let receiver = receivers.get(event.id);
        if (!receiver) {
          receiver = operationProgressReceiver(
            operationsRef.current.find((item) => item.id === event.id) || null,
            (operation) => {
              if (active)
                setOperations((current) =>
                  [
                    ...current.filter((item) => item.id !== operation.id),
                    operation,
                  ].sort((a, b) => b.order - a.order),
                );
            },
            api.getOperation,
            (message) => {
              if (active) setError(message);
            },
          );
          receivers.set(event.id, receiver);
        }
        receiver.accept(event);
      })
      .then((unlisten) => {
        if (active) detach = unlisten;
        else unlisten();
      })
      .catch((reason) => {
        if (active) setError(String(reason));
      });
    return () => {
      active = false;
      receivers.forEach((receiver) => receiver.stop());
      detach?.();
    };
  }, []);
  const act = async () => {
    if (!confirm || busy) return;
    setBusyKind("operation");
    setCleanupPlan(null);
    setBusy(confirm.id);
    setError("");
    try {
      const updated =
        confirm.action === "undo"
          ? await api.undoOperation(confirm.id)
          : await api.recoverOperation(confirm.id, confirm.action);
      // Retain the native result before saving the renderer's refreshed catalog.
      setOperations((current) =>
        current.map((operation) =>
          operation.id === updated.id ? updated : operation,
        ),
      );
      setConfirm(null);
      await onComplete(updated);
      reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy("");
    }
  };
  const previewCleanup = async (id: string) => {
    if (busy) return;
    setConfirm(null);
    setCleanupPlan(null);
    setCleanupApproved(false);
    setBusyKind("preview");
    setBusy(id);
    setError("");
    try {
      setCleanupPlan(await api.planCleanup(id));
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy("");
    }
  };
  const clean = async () => {
    if (!cleanupPlan || !cleanupApproved || busy) return;
    setBusyKind("cleanup");
    setBusy(cleanupPlan.operationId);
    setError("");
    setOperations((current) =>
      current.map((operation) =>
        operation.id === cleanupPlan.operationId
          ? { ...operation, cleanup: { ...cleanupPlan, status: "deleting" } }
          : operation,
      ),
    );
    try {
      const receipt = await api.cleanupHistory(
        cleanupPlan.operationId,
        cleanupPlan.id,
      );
      setOperations((current) =>
        current.map((operation) =>
          operation.id === receipt.id ? receipt : operation,
        ),
      );
      setCleanupPlan(null);
      setCleanupApproved(false);
    } catch (reason) {
      setError(String(reason));
      setCleanupApproved(false);
      // Cleanup may have durably removed some backups before returning an error.
      // Refresh the receipt before offering any reversal or retry.
      setCleanupPlan(null);
      try {
        setOperations(await api.listOperations());
      } catch {
        /* Preserve the original actionable error and existing history. */
      }
    } finally {
      setBusy("");
    }
  };
  return (
    <Modal
      title="File operation history"
      onClose={() => {
        if (!busy) onClose();
      }}
      wide
      closeDisabled={!!busy}
      footer={
        <button disabled={!!busy} onClick={onClose}>
          Close
        </button>
      }
    >
      <p className="muted">
        File changes have their own recovery history. Comparison undo and review
        undo never reverse files.
      </p>
      {error && (
        <>
          <p className="error" role="alert">
            {error}
          </p>
          <button disabled={!!busy} onClick={reload}>
            Reload operation history
          </button>
        </>
      )}
      {busy && (
        <div className="notice">
          <LoaderCircle size={17} className="spin" />
          <span>
            {busyKind === "preview"
              ? "Checking retained recovery files…"
              : busyKind === "cleanup"
                ? "Removing approved recovery files. This window stays open until a safe boundary."
                : "Applying journaled changes. This window stays open until a safe boundary."}
          </span>
          {busyKind !== "preview" && (
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
        </div>
      )}
      {loading && (
        <p role="status" className="muted">
          Loading file operation history…
        </p>
      )}
      {!loading &&
        !error &&
        !operations.filter(
          (operation) =>
            !recoveryOnly ||
            !["planned", "complete", "rolled-back", "abandoned"].includes(
              operation.status,
            ),
        ).length && (
          <div className="empty-preview">
            <CheckCircle2 size={26} />
            <span>
              {desktop
                ? "No file operations yet."
                : "File operations are available in the desktop app."}
            </span>
          </div>
        )}
      {operations
        .filter(
          (operation) =>
            !recoveryOnly ||
            !["planned", "complete", "rolled-back", "abandoned"].includes(
              operation.status,
            ),
        )
        .map((op) => (
          <div className="operation-card" key={op.id}>
            <div className="row-between">
              <div>
                <strong>
                  {op.context?.sourceGroup
                    ? "Move to subfolder"
                    : op.kind === "rename"
                      ? "Rename"
                      : "Export"}{" "}
                  · {op.rows.length} files
                </strong>
                <small>{op.id}</small>
              </div>
              <span
                className={`badge ${op.status === "complete" ? "success" : ""}`}
              >
                {op.status === "abandoned"
                  ? "Files kept · dismissed"
                  : op.historyRetired
                    ? "History retired"
                    : op.status}
              </span>
            </div>
            <p className="muted">
              {op.completed} steps completed
              {op.manifestPath ? " · Manifest saved" : ""}
            </p>
            {op.exfatOutputs && (
              <ExfatNotice
                dismissed={op.status === "abandoned"}
                sourceChange={op.kind === "rename"}
                movedGroup={!!op.context?.sourceGroup}
              />
            )}
            {op.errors.map((err, i) => (
              <p key={i} className="error">
                {err}
              </p>
            ))}
            <div className="button-row">
              {op.status === "complete" &&
                !op.historyRetired &&
                (!op.cleanup || op.cleanup.status === "planned") && (
                  <button
                    disabled={!!busy}
                    onClick={() => {
                      setCleanupPlan(null);
                      setConfirm({ id: op.id, action: "undo" });
                    }}
                  >
                    Undo file operation
                  </button>
                )}
              {!op.historyRetired &&
                !["complete", "rolled-back", "planned", "abandoned"].includes(
                  op.status,
                ) && (
                  <>
                    <button
                      disabled={!!busy || op.status === "rolling-back"}
                      onClick={() =>
                        setConfirm({ id: op.id, action: "resume" })
                      }
                    >
                      {["files-committed", "artifact-repair-only"].includes(
                        op.status,
                      )
                        ? "Finish manifests"
                        : "Resume operation"}
                    </button>
                    <button
                      disabled={!!busy}
                      onClick={() =>
                        setConfirm({ id: op.id, action: "rollback" })
                      }
                    >
                      Roll back safely
                    </button>
                    {op.exfatOutputs && (
                      <button
                        disabled={!!busy}
                        onClick={() => {
                          setCleanupPlan(null);
                          setConfirm({ id: op.id, action: "abandon" });
                        }}
                      >
                        {op.kind === "rename"
                          ? "Keep files where they are"
                          : "Keep files and dismiss export"}
                      </button>
                    )}
                  </>
                )}
              {!op.historyRetired &&
                !op.exfatOutputs &&
                ["complete", "rolled-back"].includes(op.status) && (
                  <button
                    disabled={!!busy}
                    onClick={() => void previewCleanup(op.id)}
                  >
                    {op.cleanup && op.cleanup.status !== "planned"
                      ? "Review interrupted cleanup"
                      : "Preview history cleanup"}
                  </button>
                )}
            </div>
            {op.historyRetired && op.status !== "abandoned" && (
              <p className="notice">
                Recovery history was retired. File undo is unavailable. Your
                current images, completed outputs, and manifests remain in
                place; this receipt is retained.
              </p>
            )}
            {!op.historyRetired &&
              !op.exfatOutputs &&
              op.cleanup &&
              op.cleanup.status !== "planned" && (
                <p className="notice">
                  History cleanup was interrupted. Some recovery files may
                  already be removed, so file undo is unavailable. Review the
                  remaining cleanup to continue.
                </p>
              )}
            {!op.exfatOutputs &&
              op.status !== "abandoned" &&
              cleanupPlan?.operationId === op.id && (
                <section className="cleanup-preview">
                  <h3>History cleanup preview</h3>
                  <p>
                    {cleanupPlan.files.length} recovery files ·{" "}
                    {bytes(cleanupPlan.totalBytes)} recorded size
                  </p>
                  <p className="notice">
                    This permanently removes the listed recovery backups and
                    retained partial files, then retires the operation journal.
                    File undo for this operation will be lost. Current images,
                    completed copies, and final manifests are kept.
                  </p>
                  {cleanupPlan.files.length ? (
                    <div className="table-scroll">
                      <table>
                        <thead>
                          <tr>
                            <th>Recovery file to remove</th>
                            <th>Size</th>
                            <th>Status</th>
                          </tr>
                        </thead>
                        <tbody>
                          {cleanupPlan.files.map((file) => (
                            <tr key={file.path}>
                              <td>{file.path}</td>
                              <td>{bytes(file.sizeBytes)}</td>
                              <td>{file.status}</td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  ) : (
                    <p className="muted">
                      No backup or partial files need removal. The operation
                      journal will still be retired and file undo will be lost.
                    </p>
                  )}
                  <label className="check-label confirm">
                    <input
                      type="checkbox"
                      checked={cleanupApproved}
                      disabled={!!busy}
                      onChange={(event) =>
                        setCleanupApproved(event.target.checked)
                      }
                    />
                    I reviewed these files and accept permanently losing file
                    undo for this operation.
                  </label>
                  <div className="button-row">
                    <button
                      disabled={!!busy}
                      onClick={() => {
                        setCleanupPlan(null);
                        setCleanupApproved(false);
                      }}
                    >
                      Keep history
                    </button>
                    <button
                      className="danger"
                      disabled={!!busy || !cleanupApproved}
                      onClick={() => void clean()}
                    >
                      {busy
                        ? "Cleaning…"
                        : "Delete recovery files and retire history"}
                    </button>
                  </div>
                </section>
              )}
            {!!op.dependencies?.length && (
              <p className="muted">
                Depends on {op.dependencies.length} earlier file operations.
                Undo newer dependent operations first.
              </p>
            )}
            {op.status !== "abandoned" && confirm?.id === op.id && (
              <div className="notice">
                <p>
                  {confirm.action === "abandon"
                    ? op.kind === "rename"
                      ? op.context?.sourceGroup
                        ? "Keep every image in its current location and dismiss this unfinished move. This ends Undo and Resume for this move. No files will be deleted or moved."
                        : "Keep every image with its current name and location and dismiss this unfinished rename. This ends Undo and Resume for this rename. No files will be deleted, renamed or moved."
                      : "Keep all completed and partial export files and dismiss this unfinished export. This ends Undo for this export and lets you start a fresh export. No files will be deleted."
                    : confirm.action === "undo"
                      ? "Restore the prior file paths or remove verified exported copies. Changed or occupied files will block unsafe reversal."
                      : confirm.action === "resume"
                        ? "Resume unfinished changes after verifying recorded file identities."
                        : "Reverse completed changes after verifying recorded file identities."}
                </p>
                <div className="button-row">
                  <button disabled={!!busy} onClick={() => setConfirm(null)}>
                    Cancel
                  </button>
                  <button className="primary" disabled={!!busy} onClick={act}>
                    {busy
                      ? "Working…"
                      : confirm.action === "abandon"
                        ? "Confirm keep files and dismiss"
                        : `Confirm ${confirm.action}`}
                  </button>
                </div>
              </div>
            )}
            <details>
              <summary>View all affected files</summary>
              <OperationRows operation={op} />
            </details>
          </div>
        ))}
    </Modal>
  );
}
