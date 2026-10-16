import { useEffect, useRef, useState } from "react";
import type {
  BootStatus,
  ProfileDisposition,
  RecoveryReplacementPreview,
  RecoveryReplacementSubject,
} from "../native";
import { Modal } from "./Modal";
import {
  RecoveryWorkControls,
  type RecoveryWorkActions,
} from "./RecoveryWorkControls";

export interface RecoveryReplacementCommands {
  prepare: (
    subject: RecoveryReplacementSubject,
    expectedCurrentSession?: unknown,
  ) => Promise<RecoveryReplacementPreview>;
  cancel: (token: string) => Promise<void>;
  exportCopy: (token: string) => Promise<string | null>;
  confirm: (
    subject: RecoveryReplacementSubject,
    token: string,
    disposition: ProfileDisposition,
  ) => Promise<BootStatus>;
}
type Phase =
  | "preparing"
  | "review"
  | "retrying"
  | "exporting"
  | "confirming"
  | "cancelling"
  | "uncertain"
  | "committed";
const precommitCodes = new Set([
  "STALE_RECOVERY_PREVIEW",
  "STALE_RECOVERY_REPLACEMENT",
  "RECOVERY_SAVE_REQUIRED",
  "RECOVERY_COPY_REQUIRED",
  "RECOVERY_CURRENT_UI_UNADMITTED",
  "INVALID_RECOVERY_DISPOSITION",
  "RECOVERY_BUSY",
  // Native guarantees this code only before its activation seal. Post-seal
  // failures remain ACTIVATION_RECOVERY_REQUIRED/unknown and retain exact retry.
  "RECOVERY_CANCELLED",
]);

/** App freezes current edits before mounting; imported consent belongs to subject. */
export function RecoveryReplacement(props: {
  subject: RecoveryReplacementSubject;
  generation: number;
  commands: RecoveryReplacementCommands;
  flush: () => Promise<void>;
  retry: () => Promise<void>;
  snapshot: () => unknown | undefined;
  canRetrySave: boolean;
  onClose: () => void;
  onRestored: (status: BootStatus) => void | Promise<void>;
  work?: RecoveryWorkActions;
}) {
  const latest = useRef(props);
  latest.current = props;
  const alive = useRef(false),
    initial = useRef<Promise<void> | null>(null),
    occupied = useRef(true),
    epoch = useRef(0),
    held = useRef<RecoveryReplacementPreview | null>(null),
    confirmed = useRef<BootStatus | null>(null);
  const confirmation = useRef<{
    token: string;
    disposition: ProfileDisposition;
  } | null>(null);
  const [preview, setPreview] = useState<RecoveryReplacementPreview | null>(
      null,
    ),
    [phase, setPhase] = useState<Phase>("preparing"),
    [error, setError] = useState(""),
    [saveError, setSaveError] = useState(""),
    [exported, setExported] = useState(""),
    [discard, setDiscard] = useState(false);
  const transition = (next: Phase) => {
    occupied.current = !["review", "uncertain", "committed"].includes(next);
    setPhase(next);
  };
  const valid = (id: number, generation: number) =>
    alive.current &&
    epoch.current === id &&
    latest.current.generation === generation;
  const releaseAuthorization = (token: string) => {
    void latest.current.commands
      .cancel(token)
      .catch((reason) =>
        console.warn("Recovery replacement release failed", reason),
      );
  };
  const publish = async (status: BootStatus) => {
    confirmed.current = status;
    held.current = null;
    transition("committed");
    await latest.current.onRestored(status);
  };
  // FIX 1: Keep exact confirmation on unknown/lost response. Do not reprepare,
  // unfreeze or replace its disposition until native proves a precommit failure.
  const confirm = async (
    candidate: RecoveryReplacementPreview,
    disposition: ProfileDisposition,
    id: number,
    generation: number,
  ) => {
    transition("confirming");
    confirmation.current = { token: candidate.token, disposition };
    try {
      const result = await latest.current.commands.confirm(
        latest.current.subject,
        candidate.token,
        disposition,
      );
      if (valid(id, generation)) await publish(result);
    } catch (cause) {
      if (!valid(id, generation)) return;
      setError(String(cause));
      if (confirmed.current) {
        transition("committed");
        return;
      }
      const code = /^(?:Error:\s*)?([A-Z][A-Z0-9_]+)(?::|$)/.exec(
        String(cause),
      )?.[1];
      if (code && precommitCodes.has(code)) {
        confirmation.current = null;
        transition("review");
      } else transition("uncertain");
    }
  };
  const prepare = async (retry = false, initializing = false) => {
    if (occupied.current && !initializing) return;
    const id = ++epoch.current,
      generation = latest.current.generation;
    transition(retry ? "retrying" : "preparing");
    setError("");
    setExported("");
    setDiscard(false);
    setPreview(null);
    confirmation.current = null;
    let failed = "";
    try {
      if (retry) {
        try {
          await latest.current.retry();
        } catch (cause) {
          failed = String(cause);
        }
      }
      try {
        await latest.current.flush();
      } catch (cause) {
        failed ||= String(cause);
      }
      if (!valid(id, generation)) return;
      setSaveError(failed);
      const expected = latest.current.snapshot();
      const next = await latest.current.commands.prepare(
        latest.current.subject,
        expected === undefined ? undefined : structuredClone(expected),
      );
      if (!valid(id, generation)) {
        releaseAuthorization(next.token);
        return;
      }
      if (
        next.subjectToken !== latest.current.subject.token ||
        next.subjectKind !== latest.current.subject.kind ||
        !next.token
      ) {
        if (next.token) releaseAuthorization(next.token);
        throw new Error(
          "STALE_RECOVERY_REPLACEMENT: native authorization belongs to another restore",
        );
      }
      held.current = next;
      setPreview(next);
      transition("review");
      if (
        !next.requiresDisposition &&
        !next.rendererChangesUnadmitted &&
        (!next.hasCurrentSession || next.currentSaveState === "saved")
      )
        await confirm(next, "saved", id, generation);
    } catch (cause) {
      if (valid(id, generation)) {
        setError(String(cause));
        transition("review");
      }
    }
  };
  useEffect(() => {
    alive.current = true;
    if (!initial.current) initial.current = prepare(false, true);
    return () => {
      alive.current = false;
      queueMicrotask(() => {
        if (
          !alive.current &&
          held.current &&
          !confirmation.current &&
          !confirmed.current
        ) {
          const token = held.current.token;
          held.current = null;
          releaseAuthorization(token);
        }
      });
    };
  }, []);
  const cancel = async () => {
    if (occupied.current || confirmation.current || confirmed.current) return;
    const id = ++epoch.current,
      generation = latest.current.generation;
    transition("cancelling");
    setError("");
    try {
      if (held.current)
        await latest.current.commands.cancel(held.current.token);
      if (valid(id, generation)) {
        held.current = null;
        latest.current.onClose();
      }
    } catch (cause) {
      if (valid(id, generation)) {
        setError(String(cause));
        transition("review");
      }
    }
  };
  // FIX 2: A native export is token-bound and never receives a placeholder or
  // a renderer value that native has not admitted. Imported consent is unchanged.
  const restoreWith = async (
    disposition: ProfileDisposition,
    makeCopy = false,
  ) => {
    if (
      occupied.current ||
      !held.current ||
      confirmed.current ||
      confirmation.current ||
      (disposition === "discard" && !discard) ||
      (disposition === "recovery-copy" &&
        (held.current.rendererChangesUnadmitted || (!makeCopy && !exported)))
    )
      return;
    const id = ++epoch.current,
      generation = latest.current.generation,
      candidate = held.current;
    setError("");
    if (makeCopy) {
      transition("exporting");
      try {
        const output = await latest.current.commands.exportCopy(
          candidate.token,
        );
        if (!valid(id, generation)) return;
        if (!output) {
          setError(
            "Recovery copy cancelled. Current pending work and the inspected restore remain unchanged.",
          );
          transition("review");
          return;
        }
        setExported(output);
      } catch (cause) {
        if (valid(id, generation)) {
          setError(String(cause));
          transition("review");
        }
        return;
      }
    }
    await confirm(candidate, disposition, id, generation);
  };
  const busy = occupied.current;
  return (
    <Modal
      title="Preserve current work before restoring"
      wide
      closeDisabled={busy || phase === "uncertain" || phase === "committed"}
      onClose={() => void cancel()}
      footer={
        <button
          disabled={busy || phase === "uncertain" || phase === "committed"}
          onClick={() => void cancel()}
        >
          Keep current session
        </button>
      }
    >
      <p>
        The imported copy's pending-action choice is separate from changes in
        your current session. Previously saved records and original image files
        stay preserved.
      </p>
      <section aria-label="Imported recovery choice">
        <h3>Imported recovery</h3>
        <p>
          {props.subject.kind === "resume"
            ? "Resume the retained request using its original imported pending-action choice."
            : props.subject.acceptPending
              ? "Restore the saved state and the previously approved imported pending actions."
              : "No imported pending actions have been approved for this restore."}
        </p>
      </section>
      {busy && (
        <p role="status">
          {phase === "exporting"
            ? "Choose a destination for the current session's recovery copy. Cancelling keeps current work in memory."
            : phase === "retrying"
              ? "Retrying the current save, then preparing new native replacement authorization…"
              : phase === "confirming"
                ? "Native recovery is confirming the exact prepared replacement…"
                : phase === "cancelling"
                  ? "Releasing this replacement authorization…"
                  : "Waiting for queued saves and checking the current native session…"}
        </p>
      )}
      {preview && (
        <section aria-label="Current session replacement preview">
          <h3>Current session</h3>
          <p>
            {preview.hasCurrentSession
              ? `Save state: ${preview.currentSaveState}.`
              : "No current native session is available. Existing storage records remain preserved."}
          </p>
          {!!preview.pendingLayers.length && (
            <ul>
              {preview.pendingLayers.map((layer) => (
                <li key={`${layer.kind}:${layer.id}`}>
                  <strong>
                    {layer.kind === "pending-event"
                      ? "Unacknowledged event"
                      : "Later pending edits"}
                  </strong>
                  : {layer.summary} <code>{layer.id}</code>
                </li>
              ))}
            </ul>
          )}
          {preview.rendererChangesUnadmitted && (
            <p className="notice">
              The latest visible edits have not been admitted by native storage.
              A native recovery copy cannot include those edits. Retry saving
              them or explicitly discard them before restoring.
            </p>
          )}
          {saveError && preview.hasCurrentSession && (
            <p className="notice">Current save: {saveError}</p>
          )}
          {!!preview.warnings.length && (
            <ul>
              {preview.warnings.map((warning, index) => (
                <li key={index}>{warning}</li>
              ))}
            </ul>
          )}
          {exported && (
            <p className="notice">
              Current recovery copy saved: {exported}. Native confirmation must
              still verify this copy against the exact replacement.
            </p>
          )}
        </section>
      )}
      {error && (
        <p role="alert" className="error">
          {error}
        </p>
      )}
      {phase === "review" && (
        <>
          <div className="button-row">
            <button
              disabled={!props.canRetrySave}
              onClick={() => void prepare(true)}
            >
              Retry current save and restore
            </button>
            <button onClick={() => void prepare()}>
              Refresh replacement authorization
            </button>
            {preview?.hasCurrentSession &&
              !preview.rendererChangesUnadmitted && (
                <button onClick={() => void restoreWith("recovery-copy", true)}>
                  Save current recovery copy and restore
                </button>
              )}
            {exported && !preview?.rendererChangesUnadmitted && (
              <button onClick={() => void restoreWith("recovery-copy")}>
                Restore using saved current recovery copy
              </button>
            )}
          </div>
          {preview && (
            <div className="close-discard">
              <label className="check-label">
                <input
                  type="checkbox"
                  checked={discard}
                  onChange={(event) => setDiscard(event.target.checked)}
                />
                I accept losing only the current unsaved edits. Keep previously
                saved records and files.
              </label>
              <button
                className="danger"
                disabled={!discard}
                onClick={() => void restoreWith("discard")}
              >
                Discard current unsaved edits and restore
              </button>
            </div>
          )}
        </>
      )}
      {/* FIX 3: Lost confirmation has only exact-token retry; committed reload
        failure has only reload. Neither path can repeat preparation or discard. */}
      {phase === "uncertain" && (
        <>
          <p className="notice">
            The restore outcome is not confirmed. Current editing stays frozen.
            Check the same native request before taking another action.
          </p>
          <button
            onClick={() => {
              if (!confirmation.current || !held.current || occupied.current)
                return;
              setError("");
              void confirm(
                held.current,
                confirmation.current.disposition,
                ++epoch.current,
                latest.current.generation,
              );
            }}
          >
            Check restore outcome
          </button>
        </>
      )}
      {phase === "committed" && (
        <>
          <p className="notice">
            The native branch is selected. Reload its workspace to continue.
          </p>
          <button
            onClick={async () => {
              if (!confirmed.current) return;
              try {
                await latest.current.onRestored(confirmed.current);
              } catch (cause) {
                if (alive.current) setError(String(cause));
              }
            }}
          >
            Reload selected workspace
          </button>
        </>
      )}
      {props.work && (
        <RecoveryWorkControls
          actions={props.work}
          watch={busy || phase === "uncertain"}
        />
      )}
    </Modal>
  );
}
