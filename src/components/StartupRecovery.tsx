import { useEffect, useRef, useState } from "react";
import { ProfileControls } from "./ProfileControls";
import { ProtectionStatusPanel } from "./ProtectionStatusPanel";
import {
  RecoveryStoragePanel,
  type RecoveryStorageActions,
} from "./RecoveryStoragePanel";
import type {
  PendingProfileSwitch,
  ProfileDisposition,
  ProfileSummary,
} from "../native";
import {
  RecoveryWorkControls,
  type RecoveryWorkActions,
} from "./RecoveryWorkControls";
import {
  AlertTriangle,
  FolderOpen,
  LoaderCircle,
  RotateCcw,
  ShieldCheck,
} from "lucide-react";

export interface RecoveryBootView {
  pendingProfileSwitch?: PendingProfileSwitch;
  mode: "ready" | "recovery";
  state?: string;
  issues: Array<{ code: string; message: string }>;
  canRestoreBackup: boolean;
  pendingOperations?: number;
  trustedOperationsAvailable?: boolean;
  profile?: ProfileSummary;
  canChooseRecoveryStorage?: boolean;
}
export interface RecoveryInspectionView {
  token: string;
  createdAt: string;
  summary: {
    images: number;
    runs: number;
    decisions: number;
    reviewEdits: number;
  };
  warnings: string[];
  restorable: boolean;
  missingJournals?: string[];
  missingBackups?: string[];
  savedRevision?: number | null;
  pendingActions?: number;
  pendingSummary?: string[];
}

export function StartupRecovery({
  status,
  failure,
  onRetry,
  onOpen,
  onRestore,
  onRestored,
  onBusyChange,
  onOperations,
  onHelp,
  onMaintenance,
  onChooseStorage,
  onOpenProfile,
  onResumeProfile,
  onProfileResumed,
  storage,
  work,
  embedded = false,
}: {
  status: RecoveryBootView | null;
  failure: string;
  onRetry: () => Promise<void>;
  onOpen?: () => Promise<RecoveryInspectionView | null>;
  onRestore?: (
    token: string,
    acceptPending: boolean,
  ) => Promise<RecoveryBootView>;
  onRestored: (status: RecoveryBootView) => Promise<void> | void;
  onBusyChange?: (busy: boolean) => void;
  onOperations?: () => void;
  onHelp?: () => void;
  onMaintenance?: () => void;
  onChooseStorage?: () => void;
  onOpenProfile?: () => void;
  onResumeProfile?: (
    token: string,
    disposition: ProfileDisposition,
  ) => Promise<RecoveryBootView>;
  onProfileResumed?: (status: RecoveryBootView) => Promise<void> | void;
  storage?: RecoveryStorageActions;
  work?: RecoveryWorkActions;
  embedded?: boolean;
}) {
  const [busy, setBusy] = useState<
    "" | "retry" | "inspect" | "restore" | "profile"
  >("");
  const [inspection, setInspection] = useState<RecoveryInspectionView | null>(
    null,
  );
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [acceptPending, setAcceptPending] = useState(false);
  const [storageBusy, setStorageBusy] = useState(false);
  const [nativeWorkBusy, setNativeWorkBusy] = useState(!!work);
  const fixedSelection = useRef<PendingProfileSwitch | null>(null);
  const profileCommand = useRef(false);
  const [profileCommitted, setProfileCommitted] = useState(false);
  const [profileAttempted, setProfileAttempted] = useState(false);
  const [workRefresh, setWorkRefresh] = useState(0);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const selectedProfile =
    fixedSelection.current || status?.pendingProfileSwitch;
  const workerUnavailable = !!busy || storageBusy || nativeWorkBusy;
  const unavailable = workerUnavailable || profileAttempted;
  // FIX 2: retry an uncertain confirmation with its original token AND disposition.
  const resumeProfile = async () => {
    if (
      !selectedProfile ||
      !onResumeProfile ||
      workerUnavailable ||
      profileCommand.current ||
      profileCommitted
    )
      return;
    const intent = (fixedSelection.current ||=
      structuredClone(selectedProfile));
    profileCommand.current = true;
    setProfileAttempted(true);
    setBusy("profile");
    setError("");
    try {
      const result = await onResumeProfile(intent.token, intent.disposition);
      if (!alive.current) return;
      // Native returns only after dropping the worker. A failed navigation must
      // not turn a known committed selection into another confirmation.
      setProfileCommitted(true);
      await onProfileResumed?.(result);
    } catch (reason) {
      if (alive.current) {
        setError(String(reason));
        if (work) {
          setNativeWorkBusy(true);
          setWorkRefresh((value) => value + 1);
        }
      }
    } finally {
      profileCommand.current = false;
      if (alive.current) setBusy("");
    }
  };
  useEffect(() => {
    onBusyChange?.(unavailable);
  }, [unavailable, onBusyChange]);
  useEffect(() => () => onBusyChange?.(false), [onBusyChange]);
  const restricted = status?.mode === "recovery" || !!failure;
  const open = async () => {
    if (!onOpen || unavailable) return;
    setBusy("inspect");
    setError("");
    setMessage("");
    try {
      const result = await onOpen();
      if (result) {
        setInspection(result);
        setAcceptPending(false);
      } else
        setMessage(
          "No recovery copy selected. Your current data is unchanged.",
        );
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy("");
    }
  };
  const restore = async () => {
    if (
      !inspection?.restorable ||
      status?.canRestoreBackup === false ||
      !onRestore ||
      unavailable ||
      ((inspection.pendingActions || 0) > 0 && !acceptPending)
    )
      return;
    setBusy("restore");
    setError("");
    setMessage("");
    try {
      const result = await onRestore(inspection.token, acceptPending);
      await onRestored(result);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy("");
    }
  };
  return (
    <section
      className={`startup-recovery ${embedded ? "embedded" : ""}`}
      aria-busy={unavailable}
    >
      <div className="recovery-intro">
        <span className="recovery-symbol">
          {restricted ? <AlertTriangle size={28} /> : <ShieldCheck size={28} />}
        </span>
        <div>
          <h1>
            {restricted
              ? "Your workspace needs recovery"
              : "Open a session recovery copy"}
          </h1>
          <p>
            {restricted
              ? "Your original records are preserved. Image changes and ranking edits stay unavailable until a valid workspace is restored."
              : "Inspect the saved session and its limitations before restoring it into a new branch."}
          </p>
        </div>
      </div>
      {failure && (
        <p className="error" role="alert">
          {failure}
        </p>
      )}
      {(onChooseStorage || onOpenProfile || status?.profile) && (
        <ProfileControls
          profile={status?.profile}
          disabled={unavailable || status?.canChooseRecoveryStorage === false}
          onChoose={onChooseStorage}
          onOpen={onOpenProfile}
        />
      )}
      {!!status?.issues.length && (
        <ul className="recovery-issues">
          {status.issues.map((issue, index) => (
            <li key={`${issue.code}-${index}`}>
              <strong>{issue.code.replaceAll("-", " ")}</strong>
              <p>{issue.message}</p>
            </li>
          ))}
        </ul>
      )}
      {selectedProfile && onResumeProfile && (
        <div className="recovery-inspection">
          <h2>Recorded profile selection</h2>
          <p>{selectedProfile.profile.displayPath}</p>
          <p>
            Recorded current-work decision:{" "}
            {selectedProfile.disposition === "recovery-copy"
              ? "use the verified recovery copy"
              : selectedProfile.disposition === "discard"
                ? "discard the previously approved unsaved changes"
                : "keep the saved state"}
            .
          </p>
          {selectedProfile.warnings.map((warning, index) => (
            <p className="notice" key={index}>
              {warning}
            </p>
          ))}
          <p className="muted">
            Resume this exact recorded selection. Native recovery rechecks its
            original authorization; no new discard decision is implied.
          </p>
          <button
            className="primary"
            disabled={workerUnavailable || profileCommitted}
            onClick={resumeProfile}
          >
            Resume selected profile
          </button>
          {profileCommitted && (
            <p role="status">
              The selected profile was confirmed. Reload the selected profile to
              continue.
            </p>
          )}
        </div>
      )}
      {!!status?.pendingOperations && (
        <p className="notice">
          {status.pendingOperations} file operations are pending. Their trusted
          journals must be available before they can resume or roll back.
        </p>
      )}
      {status?.canRestoreBackup && (
        <p className="muted">
          This profile permits restoring a validated recovery copy into a new
          session branch. Existing records remain preserved.
        </p>
      )}
      <div className="button-row recovery-actions">
        {onHelp && <button onClick={onHelp}>Help and local diagnostics</button>}
        {onMaintenance && (
          <button disabled={unavailable} onClick={onMaintenance}>
            Review protection storage maintenance
          </button>
        )}
        {onOperations && (
          <button disabled={unavailable} onClick={onOperations}>
            Recover file operations
          </button>
        )}
        <button
          disabled={unavailable}
          onClick={async () => {
            setBusy("retry");
            setError("");
            try {
              await onRetry();
            } catch (reason) {
              setError(String(reason));
            } finally {
              setBusy("");
            }
          }}
        >
          {busy === "retry" ? (
            <LoaderCircle className="spin" size={17} />
          ) : (
            <RotateCcw size={17} />
          )}{" "}
          Retry loading
        </button>
        {onOpen && (
          <button className="primary" disabled={unavailable} onClick={open}>
            {busy === "inspect" ? (
              <LoaderCircle className="spin" size={17} />
            ) : (
              <FolderOpen size={17} />
            )}{" "}
            {inspection
              ? "Inspect another recovery copy"
              : "Open recovery copy"}
          </button>
        )}
      </div>
      {restricted && (
        <p className="muted recovery-reconnect">
          If a drive or folder is disconnected, reconnect it in your operating
          system and retry. A restored copy requires choosing its source folders
          again in the app.
        </p>
      )}
      {message && (
        <p className="notice" role="status">
          {message}
        </p>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {inspection && (
        <div className="recovery-inspection">
          <div className="row-between">
            <h2>Recovery copy preview</h2>
            <span className="badge">
              {inspection.restorable
                ? "Validated for session restore"
                : "Read-only inspection"}
            </span>
          </div>
          <p className="muted">
            Created {inspection.createdAt}
            {inspection.savedRevision !== undefined &&
            inspection.savedRevision !== null
              ? ` · Saved revision ${inspection.savedRevision}`
              : ""}
          </p>
          <dl className="recovery-counts">
            <div>
              <dt>Images referenced</dt>
              <dd>{inspection.summary.images}</dd>
            </div>
            <div>
              <dt>Ranking runs</dt>
              <dd>{inspection.summary.runs}</dd>
            </div>
            <div>
              <dt>Retained decisions</dt>
              <dd>{inspection.summary.decisions}</dd>
            </div>
            <div>
              <dt>Review edits</dt>
              <dd>{inspection.summary.reviewEdits}</dd>
            </div>
          </dl>
          {(inspection.pendingActions || 0) > 0 && (
            <div className="pending-actions">
              <h3>
                {inspection.pendingActions} captured action
                {inspection.pendingActions === 1 ? "" : "s"} without a durable
                acknowledgment
              </h3>
              <p>
                These actions are separate from the saved revision. Recovery
                checks their identity and payload before applying any action
                once.
              </p>
              {inspection.pendingSummary?.map((summary, index) => (
                <p key={index}>{summary}</p>
              ))}
              <label className="check-label">
                <input
                  type="checkbox"
                  checked={acceptPending}
                  onChange={(event) => setAcceptPending(event.target.checked)}
                  disabled={unavailable || !inspection.restorable}
                />{" "}
                Restore the saved state and apply the displayed pending action
                once after validation.
              </label>
            </div>
          )}
          {!!inspection.warnings.length && (
            <ul className="recovery-issues">
              {inspection.warnings.map((warning, index) => (
                <li key={index}>{warning}</li>
              ))}
            </ul>
          )}
          {!!inspection.missingJournals?.length && (
            <div className="notice recovery-limit">
              <h3>Trusted file journals unavailable</h3>
              <p>
                The following file operations cannot be resumed, rolled back, or
                undone from this copy alone.
              </p>
              <ul>
                {inspection.missingJournals.map((id) => (
                  <li key={id}>{id}</li>
                ))}
              </ul>
            </div>
          )}
          {!!inspection.missingBackups?.length && (
            <div className="notice recovery-limit">
              <h3>Recovery backup files unavailable</h3>
              <p>
                Replacement-file backups are not included in a session recovery
                copy. Locate the original backup bytes before attempting those
                file reversals.
              </p>
              <ul>
                {inspection.missingBackups.map((id) => (
                  <li key={id}>{id}</li>
                ))}
              </ul>
            </div>
          )}
          <p className="muted">
            This copy contains session metadata and retained history. It does
            not contain your image files or overwrite-backup bytes. Imported
            folder paths and operation references do not authorize file changes.
          </p>
          <div className="restore-action">
            <p>
              A new session branch will be created. Reconnect source folders and
              verify unchanged image content before resuming comparisons.
            </p>
            <button
              className="primary"
              disabled={
                unavailable ||
                !inspection.restorable ||
                status?.canRestoreBackup === false ||
                !onRestore ||
                ((inspection.pendingActions || 0) > 0 && !acceptPending)
              }
              onClick={restore}
            >
              {busy === "restore" && (
                <LoaderCircle className="spin" size={17} />
              )}{" "}
              {busy === "restore"
                ? "Restoring session…"
                : "Restore as a new session"}
            </button>
          </div>
        </div>
      )}
      {work && (
        <RecoveryWorkControls
          actions={work}
          watch={!!busy || storageBusy}
          refreshKey={workRefresh}
          onActiveChange={setNativeWorkBusy}
        />
      )}
      {storage && (
        <RecoveryStoragePanel
          actions={storage}
          disabled={!!busy || nativeWorkBusy || profileAttempted}
          onRestored={onRestored}
          onBusyChange={setStorageBusy}
        />
      )}
      <ProtectionStatusPanel />
    </section>
  );
}
