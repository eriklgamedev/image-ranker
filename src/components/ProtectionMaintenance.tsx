import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  type ProtectionMaintenancePreview,
  type ProtectionMaintenanceResult,
  type ProtectionMaintenanceStatus,
  type RecoveryWorkStatus,
} from "../native";
import { Modal } from "./Modal";
import {
  RecoveryWorkControls,
  type RecoveryWorkActions,
} from "./RecoveryWorkControls";

export interface ProtectionMaintenanceActions {
  status: (requestId?: string) => Promise<ProtectionMaintenanceStatus>;
  preview: () => Promise<ProtectionMaintenancePreview>;
  confirm: (token: string) => Promise<ProtectionMaintenanceResult>;
  resume: (requestId: string) => Promise<ProtectionMaintenanceResult>;
  context: () => Promise<string>;
}
const nativeActions: ProtectionMaintenanceActions = {
  status: (id) => api.protectionMaintenanceStatus(id),
  preview: () => api.previewProtectionMaintenance(),
  confirm: (token) => api.confirmProtectionMaintenance(token),
  resume: (id) => api.resumeProtectionMaintenance(id),
  context: () => api.rendererContext(),
};
const storageKey = "image-ranker-protection-maintenance-request";
const validId = (value: unknown): value is string =>
  typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
const validToken = (value: string) =>
  /^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/i.test(value);
const registryKind = (kind: string) =>
  [
    "registry-status",
    "preview-registry-maintenance",
    "registry-maintenance",
  ].includes(kind);
const terminal = (state: string) =>
  ["completed", "cancelled", "failed"].includes(state);
function retainedHint() {
  try {
    const id = localStorage.getItem(storageKey);
    return validId(id) ? id : null;
  } catch {
    return null;
  }
}
const quantity = (value: number) => value.toLocaleString();

/** Always mounted by App: closing the dialog cannot release a native work freeze. */
export function ProtectionMaintenance({
  open,
  onOpen,
  onClose,
  onHelp,
  onBusyChange,
  flush,
  profileGeneration,
  work,
  actions = nativeActions,
  suspended = false,
}: {
  open: boolean;
  onOpen: () => void;
  onClose: () => void;
  onHelp: () => void;
  onBusyChange: (busy: boolean) => void;
  flush: () => Promise<void>;
  profileGeneration?: string;
  work: RecoveryWorkActions;
  actions?: ProtectionMaintenanceActions;
  suspended?: boolean;
}) {
  const latest = useRef({ actions, flush, onBusyChange, profileGeneration });
  latest.current = { actions, flush, onBusyChange, profileGeneration };
  const alive = useRef(false),
    epoch = useRef(0),
    command = useRef(false),
    observedJob = useRef<string | null>(null);
  const [requestId, setRequestId] = useState<string | null>(retainedHint),
    request = useRef(requestId);
  const [preview, setPreview] = useState<ProtectionMaintenancePreview | null>(
      null,
    ),
    [result, setResult] = useState<ProtectionMaintenanceResult | null>(null),
    [snapshot, setSnapshot] = useState<ProtectionMaintenanceStatus | null>(
      null,
    ),
    [workSnapshot, setWorkSnapshot] = useState<RecoveryWorkStatus | null>(null),
    [commandBusy, setCommandBusy] = useState(false),
    [controlsBusy, setControlsBusy] = useState(true),
    [refreshKey, setRefreshKey] = useState(0),
    [approved, setApproved] = useState(false),
    [error, setError] = useState(""),
    [saveError, setSaveError] = useState(""),
    [uncertain, setUncertain] = useState(false),
    [attempted, setAttempted] = useState(false),
    [observationDue, setObservationDue] = useState(false);
  const remember = (id: string) => {
    request.current = id;
    setRequestId(id);
    // This is only a lookup hint. Native validates every status/resume request.
    try {
      localStorage.setItem(storageKey, id);
    } catch {
      /* Native retains the authoritative request. */
    }
  };
  const observe = useCallback((value: RecoveryWorkStatus) => {
    if (value.active && registryKind(value.active.kind))
      observedJob.current = value.active.id;
    if (
      observedJob.current &&
      value.recent.some(
        (entry) => entry.id === observedJob.current && terminal(entry.state),
      )
    )
      observedJob.current = null;
    setWorkSnapshot(value);
    setObservationDue(false);
  }, []);
  const activeRegistry =
    !!workSnapshot?.active && registryKind(workSnapshot.active.kind);
  const workUnknown =
    !workSnapshot || !!observedJob.current || (controlsBusy && activeRegistry);
  const held = commandBusy || activeRegistry || workUnknown || observationDue;
  useEffect(() => {
    onBusyChange(open || held);
  }, [open, held, onBusyChange]);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      epoch.current++;
    };
  }, []);
  const valid = (id: number) => alive.current && id === epoch.current;
  const begin = () => {
    if (
      command.current ||
      workSnapshot?.active ||
      workUnknown ||
      observationDue
    )
      return null;
    command.current = true;
    setCommandBusy(true);
    latest.current.onBusyChange(true);
    return ++epoch.current;
  };
  const settled = (id: number) => {
    if (!valid(id)) return;
    command.current = false;
    setCommandBusy(false);
    setObservationDue(true);
    setControlsBusy(true);
    setRefreshKey((value) => value + 1);
  };
  const acceptResult = (
    value: ProtectionMaintenanceResult,
    expected: string,
  ) => {
    if (
      !value ||
      value.requestId !== expected ||
      !["completed", "no-reclaim"].includes(value.state)
    )
      throw new Error(
        "Protection maintenance returned a different or incomplete request outcome. Check the retained request again.",
      );
    setResult(value);
    setUncertain(false);
    setApproved(false);
    setPreview(null);
  };
  const check = async () => {
    const id = begin();
    if (id === null) return;
    setAttempted(true);
    setError("");
    setResult(null);
    setSnapshot(null);
    const expected = request.current;
    try {
      const next = await latest.current.actions.status(expected || undefined);
      if (!valid(id)) return;
      setSnapshot(next);
      if (next.error) throw new Error(next.error);
      if (!next.fresh || next.active) return;
      // FIX 1: An exact query never adopts nested lastResult or another ID.
      if (next.result) {
        if (expected && next.result.requestId !== expected)
          throw new Error(
            "Protection status belongs to another request. The retained request is still unconfirmed.",
          );
        if (!validId(next.result.requestId))
          throw new Error(
            "Native protection status has an invalid request identity.",
          );
        remember(next.result.requestId);
        acceptResult(next.result, next.result.requestId);
      } else if (
        !expected &&
        next.registry?.canResume &&
        validId(next.registry.requestId)
      ) {
        remember(next.registry.requestId);
        setUncertain(true);
      }
    } catch (cause) {
      if (valid(id)) {
        setError(String(cause));
        setSnapshot(null);
      }
    } finally {
      settled(id);
    }
  };
  const prepare = async () => {
    const id = begin();
    if (id === null) return;
    setAttempted(true);
    setError("");
    setApproved(false);
    setPreview(null);
    setResult(null);
    setSnapshot(null);
    setSaveError("");
    try {
      try {
        await latest.current.flush();
      } catch (cause) {
        if (valid(id)) setSaveError(String(cause));
      }
      if (!valid(id)) return;
      const next = await latest.current.actions.preview();
      const context = await latest.current.actions.context();
      if (!valid(id)) return;
      if (
        !validToken(next.token) ||
        !validId(next.requestId) ||
        next.contextId !== context ||
        (latest.current.profileGeneration &&
          next.profileGeneration !== latest.current.profileGeneration)
      )
        throw new Error(
          "STALE_REGISTRY_PREVIEW: this preview does not belong to the current native profile and renderer.",
        );
      remember(next.requestId);
      setPreview(next);
      setUncertain(false);
    } catch (cause) {
      if (valid(id)) setError(String(cause));
    } finally {
      settled(id);
    }
  };
  const run = async (resume = false) => {
    const exact = request.current;
    if (
      !exact ||
      (!resume && (!preview || !approved || !preview.plan.canReclaim)) ||
      (resume &&
        (!snapshot?.fresh ||
          snapshot.registry?.requestId !== exact ||
          !snapshot.registry.canResume))
    )
      return;
    const id = begin();
    if (id === null) return;
    setError("");
    setResult(null);
    setUncertain(true);
    try {
      // FIX 2: Native owns both plans and filesystem authority. Confirmation
      // sends only its opaque token; resume sends only the retained durable ID.
      const next = resume
        ? await latest.current.actions.resume(exact)
        : await latest.current.actions.confirm(preview!.token);
      if (valid(id)) acceptResult(next, exact);
    } catch (cause) {
      if (valid(id)) {
        const message = String(cause);
        setError(message);
        setApproved(false);
        if (
          message.includes("STALE_REGISTRY_PREVIEW") ||
          message.includes("maintenance preview is stale")
        ) {
          setPreview(null);
          setUncertain(false);
        } else if (/^(?:Error:\s*)?RECOVERY_CANCELLED(?::|$)/.test(message))
          setUncertain(false);
      }
    } finally {
      settled(id);
    }
  };
  const resumable =
    !!snapshot?.fresh &&
    !snapshot.active &&
    snapshot.registry?.canResume &&
    snapshot.registry.requestId === requestId;
  const disabled = held || !!workSnapshot?.active;
  // FIX 3: The independent observer survives dialog dismissal. A renderer
  // remount rediscovers native jobs; local storage is only an exact-query hint.
  const controls = (
    <RecoveryWorkControls
      actions={work}
      watch={open || commandBusy || activeRegistry || !!observedJob.current}
      refreshKey={refreshKey}
      onActiveChange={setControlsBusy}
      onSnapshot={observe}
    />
  );
  return (
    <div inert={suspended}>
      {!open && held && (
        <aside className="error-bar" role="status">
          <span>
            Protection storage work is not yet released. Session editing remains
            paused.
          </span>
          <button onClick={onOpen}>Open protection storage maintenance</button>
          <button onClick={onHelp}>Help and diagnostics</button>
        </aside>
      )}
      {!open && <div hidden={!held}>{controls}</div>}
      {open && (
        <Modal
          title="Protection storage maintenance"
          wide
          onClose={onClose}
          footer={<button onClick={onClose}>Close maintenance view</button>}
        >
          <button onClick={onHelp}>Help and diagnostics</button>
          {controls}
          <p>
            Compact shared protection records while preserving every historical
            source, path, file identity and recovery obligation. This does not
            delete image files or remove protections.
          </p>
          <p className="muted">
            Another profile uses the same shared protections; it does not reset
            this capacity.
          </p>
          {requestId && (
            <p>
              Retained maintenance request: <code>{requestId}</code>
            </p>
          )}
          <div className="button-row">
            <button disabled={disabled} onClick={() => void check()}>
              Check retained maintenance request
            </button>
            <button
              disabled={disabled || uncertain || resumable}
              onClick={() => void prepare()}
            >
              {preview
                ? "Refresh maintenance preview"
                : "Review protection maintenance"}
            </button>
          </div>
          {!attempted && (
            <p>
              Check retained native work or review a new maintenance preview. No
              storage change has been approved.
            </p>
          )}
          {commandBusy && (
            <p role="status">
              Waiting for the native protection storage command. Status and
              cancellation remain independent.
            </p>
          )}
          {snapshot && (!snapshot.fresh || snapshot.active) && (
            <p role="status">
              This is a cached observation while native work is active. It does
              not confirm completion.
            </p>
          )}
          {snapshot?.fresh && snapshot.registry && (
            <p>
              Native storage state: {snapshot.registry.state}. Generation{" "}
              {snapshot.registry.generation}.
            </p>
          )}
          {attempted &&
            requestId &&
            snapshot?.fresh &&
            !snapshot.result &&
            !resumable && (
              <p className="notice">
                No exact outcome is available for this retained request. An
                absent or expired record is not completion.
              </p>
            )}
          {saveError && (
            <p className="notice">
              Current session saving remains unacknowledged: {saveError}.
              Protection maintenance does not mark these edits saved.
            </p>
          )}
          {preview && (
            <section aria-label="Protection maintenance preview">
              <h3>Review preserved protections and space</h3>
              <dl className="support-limits">
                <dt>Recorded bytes</dt>
                <dd>{quantity(preview.plan.usedBytes)}</dd>
                <dt>Compacted size, upper bound</dt>
                <dd>{quantity(preview.plan.compactedBytesUpperBound)} bytes</dd>
                <dt>Reclaimable space, lower bound</dt>
                <dd>
                  {quantity(preview.plan.reclaimableBytesLowerBound)} bytes
                </dd>
                <dt>Preserved claims</dt>
                <dd>{quantity(preview.plan.preservedClaims)}</dd>
                <dt>Additional managed claims</dt>
                <dd>{quantity(preview.plan.addedManagedClaims)}</dd>
                <dt>Reserved completions</dt>
                <dd>
                  {quantity(preview.plan.reservedCompletionBytes)} bytes ·{" "}
                  {quantity(preview.plan.reservedCompletionClaims)} claims
                </dd>
                <dt>Extra disk space, upper bound</dt>
                <dd>{quantity(preview.plan.extraDiskBytesUpperBound)} bytes</dd>
              </dl>
              <p>
                These are conservative bounds. Native storage rechecks this
                exact generation before changing it; approval can expire.
              </p>
              {!preview.plan.canReclaim ? (
                <p className="notice">
                  No reclaimable space is available. Unique historical
                  protections and reserved completions cannot be removed.
                  Choosing another profile does not reset shared capacity.
                </p>
              ) : (
                <>
                  <label className="check-label">
                    <input
                      type="checkbox"
                      checked={approved}
                      disabled={disabled || uncertain}
                      onChange={(event) => setApproved(event.target.checked)}
                    />
                    I reviewed the preserved protections and temporary space
                    requirement.
                  </label>
                  <button
                    className="primary"
                    disabled={disabled || !approved || uncertain}
                    onClick={() => void run()}
                  >
                    Approve protection maintenance
                  </button>
                </>
              )}
            </section>
          )}
          {uncertain && !result && (
            <p className="notice">
              This request has no confirmed outcome yet. Check the exact
              retained request; do not substitute a newer result or start
              another maintenance change.
            </p>
          )}
          {resumable && (
            <>
              <p>
                Native storage can resume this exact retained request. Its
                preserved protections and recorded progress remain
                authoritative.
              </p>
              <button disabled={disabled} onClick={() => void run(true)}>
                Resume retained maintenance request
              </button>
            </>
          )}
          {result && (
            <section aria-label="Exact maintenance result">
              <h3>
                {result.state === "completed"
                  ? "Protection maintenance completed"
                  : "Protection maintenance found no reclaimable space"}
              </h3>
              <p>
                Request {result.requestId} · generation {result.generation}
              </p>
              <p>
                {quantity(result.beforeBytes)} bytes before ·{" "}
                {quantity(result.afterBytes)} bytes after ·{" "}
                {quantity(result.preservedClaims)} claims preserved.
              </p>
              <p>
                Completion reserves: {quantity(result.reservedCompletionBytes)}{" "}
                bytes · {quantity(result.reservedCompletionClaims)} claims.
              </p>
              {held && (
                <p>
                  The result is recorded. Session editing waits for native
                  resources and the command to finish.
                </p>
              )}
            </section>
          )}
          {error && (
            <p role="alert" className="error">
              {error}
            </p>
          )}
        </Modal>
      )}
    </div>
  );
}
