import { useCallback, useEffect, useRef, useState } from "react";
import { LoaderCircle, RefreshCw } from "lucide-react";
import type { RecoveryWorkJob, RecoveryWorkStatus } from "../native";

export interface RecoveryWorkActions {
  status: () => Promise<RecoveryWorkStatus>;
  cancel: (jobId: string) => Promise<{ id: string; cancelRequested: true }>;
}

const titles: Record<RecoveryWorkJob["kind"], string> = {
  "storage-status": "Inspecting recovery storage",
  "prepare-resume": "Preparing recovery resume",
  "preview-cleanup": "Preparing cleanup preview",
  cleanup: "Cleaning staged recovery files",
  "inspect-copy": "Inspecting recovery copy",
  "restore-copy": "Restoring recovery copy",
  "startup-load": "Loading local workspace",
  "prepare-profile": "Preparing profile storage",
  "switch-profile": "Switching profile",
  "registry-status": "Checking protection storage",
  "preview-registry-maintenance": "Reviewing protection storage",
  "registry-maintenance": "Maintaining protection storage",
};

const inspectionPhases: Record<string, string> = {
  admitted: "Inspection admitted",
  "awaiting-picker": "Waiting for the file picker",
  "reading-copy": "Reading recovery copy",
  "decoding-copy": "Decoding recovery copy",
  "validating-replay": "Validating retained history",
  publishing: "Publishing validated inspection",
};

const restorePhases: Record<string, string> = {
  admitted: "Restore admitted",
  "checking-current-save": "Checking the current session save",
  "checkpoint-current": "Checkpointing the current session",
  staging: "Staging the recovered session",
  bootstrap: "Building the recovered session history",
  "prepare-activation": "Preparing activation",
  committing: "Committing activation",
  publishing: "Publishing the selected session",
  published: "Finishing the selected session",
};
const restoreSealed = (job: RecoveryWorkJob | null | undefined) =>
  job?.kind === "restore-copy" &&
  ["committing", "publishing", "published"].includes(job.phase);
const profilePhases: Record<string, string> = {
  "opening-launcher": "Opening the selected storage record",
  "opening-profile": "Opening profile storage",
  "loading-session": "Loading the saved session",
  "loading-selection-request": "Loading the recorded profile selection",
  "loading-operation-history": "Loading file operation history",
  "preparing-runtime": "Preparing the workspace",
  "awaiting-picker": "Waiting for the folder picker",
  "checking-current-save": "Checking the current session save",
  "checkpoint-current": "Checkpointing the current session",
  "verifying-copy": "Verifying the current-session recovery copy",
  "preparing-selection": "Preparing profile selection",
  "committing-selection": "Committing profile selection",
  publishing: "Publishing the selected profile",
  published: "Finishing the selected profile",
  "reading-registry-status": "Reading protection maintenance status",
  "reading-protection-history": "Reading retained protection history",
  "validating-maintenance": "Validating protection maintenance",
  "finishing-maintenance": "Finishing protection maintenance",
};
const profileJob = (job: RecoveryWorkJob | null | undefined) =>
  !!job &&
  [
    "startup-load",
    "prepare-profile",
    "switch-profile",
    "registry-status",
    "preview-registry-maintenance",
    "registry-maintenance",
  ].includes(job.kind);
const sealed = (job: RecoveryWorkJob | null | undefined) =>
  restoreSealed(job) ||
  (job?.kind === "prepare-profile" && job.phase === "published") ||
  (job?.kind === "switch-profile" &&
    ["committing-selection", "publishing", "published"].includes(job.phase)) ||
  (job?.kind === "registry-maintenance" &&
    ["finishing-maintenance", "published"].includes(job.phase));

export function RecoveryWorkControls({
  actions,
  watch,
  onActiveChange,
  refreshKey = 0,
  onSnapshot,
}: {
  actions: RecoveryWorkActions;
  watch: boolean;
  onActiveChange?: (active: boolean) => void;
  refreshKey?: number;
  onSnapshot?: (snapshot: RecoveryWorkStatus) => void;
}) {
  const [snapshot, setSnapshot] = useState<RecoveryWorkStatus | null>(null);
  const [snapshotKey, setSnapshotKey] = useState(-1);
  const requestedKey = useRef(refreshKey);
  requestedKey.current = refreshKey;
  const observer = useRef(onSnapshot);
  observer.current = onSnapshot;
  const [readError, setReadError] = useState("");
  const [cancelError, setCancelError] = useState("");
  const [acknowledgedId, setAcknowledgedId] = useState<string | null>(null);
  const [observedId, setObservedId] = useState<string | null>(null);
  const [finishingId, setFinishingId] = useState<string | null>(null);
  const [finishingKind, setFinishingKind] = useState<
    RecoveryWorkJob["kind"] | null
  >(null);
  const [profileId, setProfileId] = useState<string | null>(null);
  const [reading, setReading] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [revision, setRevision] = useState(0);
  const [paused, setPaused] = useState(false);
  const alive = useRef(false);
  const generation = useRef(0);
  const readEpoch = useRef(0);
  const readInFlight = useRef(false);
  const queuedRead = useRef(false);
  const currentRefresh = useRef<() => Promise<void>>(async () => {});
  const cancelInFlight = useRef(false);
  const polls = useRef(0);
  const outcomePending =
    !!profileId &&
    !snapshot?.recent.some(
      (job) =>
        job.id === profileId &&
        ["completed", "cancelled", "failed"].includes(job.state),
    );
  const finishingPending =
    !!finishingId &&
    !snapshot?.recent.some(
      (job) =>
        job.id === finishingId &&
        ["completed", "cancelled", "failed"].includes(job.state),
    );

  const refresh = useCallback(
    async (manual = false) => {
      if (!alive.current || cancelInFlight.current) return;
      if (readInFlight.current) return;
      if (manual) {
        polls.current = 0;
        setPaused(false);
        setReadError("");
      }
      const admitted = generation.current;
      const epoch = readEpoch.current;
      const requested = requestedKey.current;
      readInFlight.current = true;
      setReading(true);
      try {
        const next = await actions.status();
        if (
          alive.current &&
          admitted === generation.current &&
          epoch === readEpoch.current
        ) {
          setSnapshot(next);
          setSnapshotKey(requested);
          if (next.active) setObservedId(next.active.id);
          if (sealed(next.active)) {
            setFinishingId(next.active!.id);
            setFinishingKind(next.active!.kind);
          }
          if (profileJob(next.active)) setProfileId(next.active!.id);
          setReadError("");
          if (requested === requestedKey.current) observer.current?.(next);
        }
      } catch (cause) {
        if (
          alive.current &&
          admitted === generation.current &&
          epoch === readEpoch.current
        ) {
          setReadError(String(cause));
          setPaused(true);
        }
      } finally {
        readInFlight.current = false;
        if (alive.current) {
          setReading(false);
          setRevision((value) => value + 1);
          if (queuedRead.current) {
            queuedRead.current = false;
            queueMicrotask(() => void currentRefresh.current());
          }
        }
      }
    },
    [actions],
  );
  currentRefresh.current = refresh;

  useEffect(() => {
    alive.current = true;
    setSnapshot(null);
    setObservedId(null);
    setAcknowledgedId(null);
    setFinishingId(null);
    setFinishingKind(null);
    setProfileId(null);
    setReadError("");
    setCancelError("");
    polls.current = 0;
    setPaused(false);
    setReading(readInFlight.current);
    setCancelling(cancelInFlight.current);
    void refresh(true);
    return () => {
      alive.current = false;
      generation.current += 1;
    };
  }, [refresh]);

  // Until native status is known, another recovery action cannot be admitted
  // safely after a renderer remount. Cancellation and status stay available.
  useEffect(() => {
    onActiveChange?.(
      !snapshot ||
        snapshotKey !== refreshKey ||
        !!snapshot.active ||
        !!readError ||
        finishingPending ||
        outcomePending,
    );
  }, [
    snapshot,
    snapshotKey,
    refreshKey,
    readError,
    finishingPending,
    outcomePending,
    revision,
    onActiveChange,
  ]);
  useEffect(() => () => onActiveChange?.(false), [onActiveChange]);

  useEffect(() => {
    if (!watch) return;
    polls.current = 0;
    setPaused(false);
    void refresh();
  }, [watch, refresh]);

  // A command may settle after the last idle observation. Discard only stale
  // reads, then obtain a fresh status without forgetting its exact job ID.
  useEffect(() => {
    if (!refreshKey) return;
    readEpoch.current += 1;
    setSnapshot(null);
    polls.current = 0;
    setPaused(false);
    if (readInFlight.current) queuedRead.current = true;
    void refresh();
  }, [refreshKey, refresh]);

  useEffect(() => {
    if (
      reading ||
      cancelling ||
      paused ||
      (!watch &&
        snapshot &&
        !snapshot.active &&
        !finishingPending &&
        !outcomePending)
    )
      return;
    if (polls.current >= 120) {
      setPaused(true);
      return;
    }
    const timer = setTimeout(() => {
      polls.current += 1;
      void refresh();
    }, 1000);
    return () => clearTimeout(timer);
  }, [
    reading,
    cancelling,
    paused,
    watch,
    snapshot,
    finishingPending,
    outcomePending,
    revision,
    refresh,
  ]);

  async function cancel(job: RecoveryWorkJob) {
    if (!alive.current || cancelInFlight.current) return;
    cancelInFlight.current = true;
    const admitted = ++generation.current;
    setCancelling(true);
    setAcknowledgedId(null);
    setCancelError("");
    try {
      const ack = await actions.cancel(job.id);
      if (!alive.current || admitted !== generation.current) return;
      if (ack.id !== job.id || !ack.cancelRequested)
        throw new Error(
          "Recovery cancellation was not acknowledged for this job.",
        );
      setAcknowledgedId(job.id);
      setSnapshot((current) =>
        current?.active?.id === job.id
          ? {
              ...current,
              active: {
                ...current.active,
                cancelRequested: true,
                state: "cancelling",
              },
            }
          : current,
      );
      polls.current = 0;
      setPaused(false);
    } catch (cause) {
      if (alive.current && admitted === generation.current) {
        setCancelError(String(cause));
        if (
          (job.kind === "restore-copy" || profileJob(job)) &&
          /^(?:Error:\s*)?RECOVERY_JOB_FINISHING(?::|$)/.test(String(cause))
        ) {
          setFinishingId(job.id);
          setFinishingKind(job.kind);
        }
      }
    } finally {
      cancelInFlight.current = false;
      if (alive.current) {
        setCancelling(false);
        setRevision((value) => value + 1);
      }
    }
  }

  const active = snapshot?.active;
  const latest = observedId
    ? snapshot?.recent.find((job) => job.id === observedId)
    : snapshot?.recent[0];
  const terminal =
    latest && ["completed", "cancelled", "failed"].includes(latest.state)
      ? latest
      : null;
  const cancellationFinished =
    acknowledgedId &&
    snapshot?.recent.some(
      (job) =>
        job.id === acknowledgedId &&
        ["completed", "cancelled", "failed"].includes(job.state),
    );
  return (
    <section
      className="recovery-work-controls"
      aria-label="Recovery work controls"
    >
      <h3>Recovery work</h3>
      {active ? (
        <>
          <p role="status">
            <LoaderCircle size={15} className="spin" /> {titles[active.kind]} ·{" "}
            {active.kind === "inspect-copy"
              ? inspectionPhases[active.phase] || active.phase
              : active.kind === "restore-copy"
                ? restorePhases[active.phase] || active.phase
                : profileJob(active)
                  ? (active.kind.includes("registry") &&
                    active.phase === "published"
                      ? "Finishing protection maintenance"
                      : profilePhases[active.phase]) || active.phase
                  : active.phase}
            {active.total !== null &&
              ` · ${active.completed} of ${active.total}${active.kind === "inspect-copy" && active.phase === "reading-copy" ? " bytes" : ""}`}
            {active.cancelRequested &&
              " · Cancellation requested; work is still active."}
          </p>
          <button
            disabled={
              cancelling ||
              active.cancelRequested ||
              sealed(active) ||
              (finishingPending && finishingId === active.id)
            }
            onClick={() => void cancel(active)}
          >
            Cancel recovery work
          </button>{" "}
        </>
      ) : finishingPending ? (
        <p role="status">
          {finishingKind?.includes("registry")
            ? "Native protection maintenance"
            : "Native activation"}{" "}
          is finishing; its terminal outcome is not yet available. Refresh to
          check the same request.
        </p>
      ) : outcomePending ? (
        <p role="status">
          The last observed workspace request has no terminal outcome yet.
          Refresh to check the same request.
        </p>
      ) : snapshot ? (
        <p className="muted">
          No recovery work is active.{" "}
          {terminal
            ? `Last request: ${terminal.state}.`
            : observedId
              ? "A terminal outcome for the last observed request is not yet available."
              : ""}
        </p>
      ) : (
        <p className="muted">Recovery work status is not yet available.</p>
      )}
      <button
        disabled={reading || cancelling}
        onClick={() => void refresh(true)}
      >
        <RefreshCw size={15} /> Refresh recovery work status
      </button>
      {active?.message && <p className="muted">{active.message}</p>}
      {profileJob(active) && (
        <p className="muted">
          {sealed(active) || (finishingPending && finishingId === active?.id)
            ? active?.kind.includes("registry")
              ? "Protection maintenance is finishing. Wait for the exact native request outcome. Cancellation cannot undo the sealed storage change."
              : "Profile selection is finishing. Wait for the native outcome; the exact confirmation and current-session decision remain fixed."
            : "Cancellation requests a safe stop. The workspace remains unavailable until native processing and its command have finished."}
        </p>
      )}
      {active?.kind === "restore-copy" && (
        <p className="muted">
          {restoreSealed(active) ||
          (finishingPending && finishingId === active.id)
            ? "Activation is finishing. Cancellation can no longer stop this selection. Wait for the native outcome; the exact restore request and chosen current-session disposition remain fixed."
            : "Cancellation requests a safe stop before activation begins. Work remains active until native storage processing stops; the current session and any staged recovery evidence stay preserved."}
        </p>
      )}
      {(active?.kind === "inspect-copy" ||
        active?.kind === "prepare-profile") &&
        active.phase === "awaiting-picker" && (
          <p className="muted">
            Close the native{" "}
            {active.kind === "prepare-profile" ? "folder" : "file"} picker to
            cancel selection. A cancellation request cannot dismiss that window;
            work remains active until it closes.
          </p>
        )}
      {active?.kind === "inspect-copy" &&
        ["decoding-copy", "validating-replay"].includes(active.phase) && (
          <p className="muted">
            Cancellation is checked at bounded validation boundaries. Work
            remains active until native processing has stopped.
          </p>
        )}
      {!active && terminal?.message && (
        <p className={terminal.state === "failed" ? "error" : "muted"}>
          {terminal.message}
        </p>
      )}
      {acknowledgedId && !cancellationFinished && (
        <p role="status">
          Cancellation requested. Recovery work must reach a safe boundary
          before it stops. Close any open file picker to let that stage finish.
        </p>
      )}
      {paused && (
        <p className="muted">
          Automatic status checks are paused. Refresh to check again.
        </p>
      )}
      {readError && (
        <p role="alert" className="error">
          Recovery work status failed: {readError}
        </p>
      )}
      {cancelError && (
        <p role="alert" className="error">
          Recovery cancellation failed: {cancelError}
        </p>
      )}
    </section>
  );
}
