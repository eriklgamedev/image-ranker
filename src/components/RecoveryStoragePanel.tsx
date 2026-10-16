import { useEffect, useRef, useState } from "react";
import type {
  BootStatus,
  RecoveryCleanupPreview,
  RecoveryResumePreview,
  RecoveryStorageStatus,
} from "../native";
import { bytes } from "./OperationDialog";

export interface RecoveryStorageActions {
  loadStatus: () => Promise<RecoveryStorageStatus>;
  prepareResume: (requestId: string) => Promise<RecoveryResumePreview | null>;
  confirmResume: (token: string) => Promise<BootStatus>;
  cancelResume: (token: string) => Promise<void>;
  previewCleanup: (requestId: string) => Promise<RecoveryCleanupPreview>;
  confirmCleanup: (token: string) => Promise<RecoveryStorageStatus>;
  cancelCleanup: (token: string) => Promise<void>;
}
type Preview =
  | { kind: "resume"; value: RecoveryResumePreview }
  | { kind: "cleanup"; value: RecoveryCleanupPreview };

export function RecoveryStoragePanel({
  actions,
  disabled = false,
  onRestored,
  onBusyChange,
}: {
  actions: RecoveryStorageActions;
  disabled?: boolean;
  onRestored: (status: BootStatus) => Promise<void> | void;
  onBusyChange?: (busy: boolean) => void;
}) {
  const [status, setStatus] = useState<RecoveryStorageStatus | null>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [committed, setCommitted] = useState<BootStatus | null>(null);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [visible, setVisible] = useState(12);
  const live = useRef(true),
    occupied = useRef(false),
    generation = useRef(0);
  const heldPreview = useRef<Preview | null>(null);
  const confirming = useRef(false);
  const actionRef = useRef(actions);
  actionRef.current = actions;

  const cancel = (item: Preview) =>
    item.kind === "resume"
      ? actionRef.current.cancelResume(item.value.token)
      : actionRef.current.cancelCleanup(item.value.token);
  const releaseLate = (item: Preview) => {
    // A retired UI cannot use this token. Release native preview memory too;
    // durable request/cleanup evidence remains available after any failure.
    void cancel(item).catch((reason) =>
      console.warn("Recovery preview release failed", reason),
    );
  };
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
      generation.current++;
      occupied.current = false;
      if (heldPreview.current && !confirming.current)
        releaseLate(heldPreview.current);
    };
  }, []);
  useEffect(() => {
    onBusyChange?.(!!busy || !!preview || !!committed);
  }, [busy, preview, committed, onBusyChange]);
  useEffect(() => () => onBusyChange?.(false), [onBusyChange]);

  async function run(
    label: string,
    operation: (current: () => boolean) => Promise<void>,
  ) {
    if (occupied.current || disabled) return;
    occupied.current = true;
    const ticket = ++generation.current;
    const current = () => live.current && generation.current === ticket;
    setBusy(label);
    setError("");
    setMessage("");
    try {
      await operation(current);
    } catch (reason) {
      if (current()) setError(String(reason));
    } finally {
      if (current()) {
        occupied.current = false;
        setBusy("");
      }
    }
  }
  const refresh = () =>
    run("Reading retained requests…", async (current) => {
      const result = await actions.loadStatus();
      if (current()) setStatus(result);
    });
  useEffect(() => {
    if (!disabled && status === null) void refresh();
  }, [actions, disabled, status]);

  function show(item: Preview) {
    heldPreview.current = item;
    setPreview(item);
  }
  function clearPreview() {
    heldPreview.current = null;
    setPreview(null);
  }
  const controlsDisabled = disabled || !!busy || !!preview || !!committed;
  return (
    <section
      className="recovery-storage"
      aria-label="Retained recovery requests"
      aria-busy={!!busy}
    >
      <div className="row-between">
        <h2>Retained recovery requests</h2>
        <button disabled={controlsDisabled} onClick={() => void refresh()}>
          Refresh recovery status
        </button>
      </div>
      <p className="muted">
        Continue an interrupted restore using its original request, or preview
        cleanup of an eligible staged branch.
      </p>
      {busy && <p role="status">{busy}</p>}
      {error && (
        <p role="alert" className="error">
          {error}
        </p>
      )}
      {message && (
        <p role="status" className="notice">
          {message}
        </p>
      )}
      {status && (
        <>
          <p>
            {status.usedBranches} of {status.limits.branches} staged branches ·{" "}
            {bytes(status.usedBytes)} of {bytes(status.limits.bytes)} retained
            storage · {status.requests.length} of {status.limits.requests}{" "}
            request records
          </p>
          <p className="muted">
            Activation: {status.activationState}. Preserved unknown files still
            count toward storage.
          </p>
          {!!status.warnings.length && (
            <ul className="recovery-issues">
              {status.warnings.map((warning, i) => (
                <li key={i}>{warning}</li>
              ))}
            </ul>
          )}
          {!status.requests.length && (
            <p>No retained recovery requests in this profile.</p>
          )}
          <ul className="recovery-request-list">
            {status.requests.slice(0, visible).map((request) => (
              <li key={request.requestId}>
                <strong>{request.state.replaceAll("-", " ")}</strong> ·{" "}
                {bytes(request.bytes)}
                <p className="muted">
                  Request <code>{request.requestId}</code>
                </p>
                {request.pendingActions > 0 && (
                  <p>
                    {request.pendingActions} captured pending actions · original
                    choice:{" "}
                    {request.acceptedPending
                      ? "include after validation"
                      : "saved state only"}
                  </p>
                )}
                {request.reason && <p className="notice">{request.reason}</p>}
                <div className="button-row">
                  <button
                    disabled={controlsDisabled || !request.canResume}
                    onClick={() =>
                      void run(
                        "Preparing retained restore…",
                        async (current) => {
                          const result = await actions.prepareResume(
                            request.requestId,
                          );
                          if (!result) {
                            if (current())
                              setMessage(
                                "No recovery copy selected. The retained request is unchanged.",
                              );
                            return;
                          }
                          const item: Preview = {
                            kind: "resume",
                            value: result,
                          };
                          if (current()) show(item);
                          else releaseLate(item);
                        },
                      )
                    }
                  >
                    Preview resume
                  </button>
                  <button
                    disabled={controlsDisabled || !request.canCleanup}
                    onClick={() =>
                      void run(
                        "Preparing cleanup preview…",
                        async (current) => {
                          const result = await actions.previewCleanup(
                            request.requestId,
                          );
                          const item: Preview = {
                            kind: "cleanup",
                            value: result,
                          };
                          if (current()) show(item);
                          else releaseLate(item);
                        },
                      )
                    }
                  >
                    Preview branch cleanup
                  </button>
                </div>
              </li>
            ))}
          </ul>
          {visible < status.requests.length && (
            <button
              disabled={controlsDisabled}
              onClick={() => setVisible((count) => count + 12)}
            >
              Show more requests
            </button>
          )}
        </>
      )}
      {preview && (
        <div
          className="recovery-inspection"
          aria-label={
            preview.kind === "resume"
              ? "Retained restore preview"
              : "Branch cleanup preview"
          }
        >
          <h3>
            {preview.kind === "resume"
              ? "Resume the original restore"
              : "Confirm staged-branch cleanup"}
          </h3>
          <p>
            Request <code>{preview.value.requestId}</code>
          </p>
          {preview.kind === "resume" ? (
            <>
              <p>
                {preview.value.action === "reopen-committed"
                  ? "Reopen the branch already selected by this request."
                  : preview.value.action === "finish-activation"
                    ? "Resolve the retained activation and select its recorded branch."
                    : "Continue restoring this request into its retained branch."}
              </p>
              <p>
                {preview.value.inspection.summary.images} images ·{" "}
                {preview.value.inspection.summary.runs} runs ·{" "}
                {preview.value.inspection.summary.decisions} retained decisions
              </p>
              <p>
                The original pending-action choice is preserved:{" "}
                {preview.value.acceptedPending
                  ? "include captured actions once after validation"
                  : "restore saved state only"}
                .
              </p>
              {preview.value.inspection.pendingSummary.map((text, i) => (
                <p key={i}>{text}</p>
              ))}
              {!!preview.value.inspection.warnings.length && (
                <ul className="recovery-issues">
                  {preview.value.inspection.warnings.map((text, i) => (
                    <li key={i}>{text}</li>
                  ))}
                </ul>
              )}
            </>
          ) : (
            <>
              <p>
                Remove {preview.value.files.length} verified files (
                {bytes(preview.value.totalBytes)}) from this eligible branch.
                This removes its staged recovery data.
              </p>
              <details>
                <summary>Files included in cleanup</summary>
                <ul>
                  {preview.value.files.map((file) => (
                    <li key={file.relativePath}>
                      <code>{file.relativePath}</code> · {bytes(file.sizeBytes)}
                    </li>
                  ))}
                </ul>
              </details>
              {!!preview.value.preserved.length && (
                <details open>
                  <summary>Files preserved</summary>
                  <ul>
                    {preview.value.preserved.map((file) => (
                      <li key={file.relativePath}>
                        <code>{file.relativePath}</code>: {file.reason}
                      </li>
                    ))}
                  </ul>
                </details>
              )}
            </>
          )}
          {!!preview.value.warnings.length && (
            <ul className="recovery-issues">
              {preview.value.warnings.map((text, i) => (
                <li key={i}>{text}</li>
              ))}
            </ul>
          )}
          <div className="button-row">
            <button
              disabled={disabled || !!busy || !!committed}
              onClick={() =>
                void run("Cancelling preview…", async (current) => {
                  await cancel(preview);
                  if (current()) clearPreview();
                })
              }
            >
              Cancel preview
            </button>
            <button
              className="primary"
              disabled={disabled || !!busy || !!committed}
              onClick={() =>
                void run(
                  preview.kind === "resume"
                    ? "Resuming retained restore…"
                    : "Cleaning verified branch files…",
                  async (current) => {
                    if (preview.kind === "resume") {
                      confirming.current = true;
                      try {
                        const result = await actions.confirmResume(
                          preview.value.token,
                        );
                        heldPreview.current = null;
                        if (!current()) return;
                        setCommitted(result);
                        setPreview(null);
                        await onRestored(result);
                      } finally {
                        confirming.current = false;
                      }
                    } else {
                      const result = await actions.confirmCleanup(
                        preview.value.token,
                      );
                      if (current()) {
                        setStatus(result);
                        clearPreview();
                        setMessage(
                          "Cleanup recorded. Preserved files remain in retained storage.",
                        );
                      }
                    }
                  },
                )
              }
            >
              {preview.kind === "resume" ? "Confirm resume" : "Confirm cleanup"}
            </button>
          </div>
        </div>
      )}
      {committed && (
        <div className="notice">
          <p>
            The native branch selection is committed. Reload the selected
            workspace to continue.
          </p>
          <button
            disabled={!!busy}
            onClick={() =>
              void run("Reloading selected workspace…", async () => {
                await onRestored(committed);
              })
            }
          >
            Reload selected workspace
          </button>
        </div>
      )}
    </section>
  );
}
