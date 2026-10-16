import { useCallback, useEffect, useRef, useState } from "react";
import { FolderOpen, LoaderCircle } from "lucide-react";
import { Modal } from "./Modal";
import {
  RecoveryWorkControls,
  type RecoveryWorkActions,
} from "./RecoveryWorkControls";
import type { RecoveryBootView } from "./StartupRecovery";
import type {
  ProfileSummary,
  ProfileSwitchPreview,
  ProfileDisposition,
} from "../native";
export type {
  ProfileSummary,
  ProfileSwitchPreview,
  ProfileDisposition,
} from "../native";
export interface ProfileBootView extends RecoveryBootView {
  profile?: ProfileSummary;
  profileGeneration?: string;
  contextId?: string;
  canChooseRecoveryStorage?: boolean;
}
export interface ProfileSwitchCommands {
  prepare: (
    mode: "create" | "open",
    expectedCurrentSession?: unknown,
  ) => Promise<ProfileSwitchPreview | null>;
  refresh: (
    token: string,
    expectedCurrentSession?: unknown,
  ) => Promise<ProfileSwitchPreview>;
  confirm: (
    token: string,
    disposition: ProfileDisposition,
  ) => Promise<ProfileBootView>;
  cancel: (token: string) => Promise<void>;
  exportCopy: (token: string) => Promise<string | null>;
}

/** The owner freezes edits synchronously before mounting this dialog. */
export function ProfileSwitcher(props: {
  mode: "create" | "open";
  current?: ProfileSummary;
  generation: number;
  commands: ProfileSwitchCommands;
  flush: () => Promise<void>;
  retry: () => Promise<void>;
  snapshot: () => unknown | undefined;
  canRetrySave?: boolean;
  work?: RecoveryWorkActions;
  onHelp?: () => void;
  onClose: () => void;
  onSwitched: (status: ProfileBootView) => Promise<void> | void;
  onReload?: () => void;
}) {
  // FIX 1: Native work owns cancellation through command settlement and its
  // exact terminal record; an idle snapshot alone cannot unfreeze a held job.
  const latest = useRef(props);
  latest.current = props;
  const mounted = useRef(false),
    committed = useRef(false),
    pendingToken = useRef<string | null>(null),
    attempt = useRef(0),
    initial = useRef<Promise<void> | null>(null),
    busyRef = useRef(true),
    nativeBusyRef = useRef(!!props.work),
    closeAfterSettlement = useRef(false),
    confirmation = useRef<{
      token: string;
      disposition: ProfileDisposition;
    } | null>(null);
  const [phase, setPhase] = useState("preparing"),
    [preview, setPreview] = useState<ProfileSwitchPreview | null>(null),
    [error, setError] = useState(""),
    [saveFailure, setSaveFailure] = useState(""),
    [exported, setExported] = useState(""),
    [approved, setApproved] = useState(false),
    [discard, setDiscard] = useState(false),
    [nativeBusy, setNativeBusy] = useState(!!props.work),
    [refreshKey, setRefreshKey] = useState(0),
    [closePending, setClosePending] = useState(false);
  const transition = (next: string) => {
    busyRef.current = !["preview", "failed", "uncertain"].includes(next);
    setPhase(next);
  };
  const workChanged = useCallback((active: boolean) => {
    nativeBusyRef.current = active;
    setNativeBusy(active);
  }, []);
  const observeSettlement = () => {
    if (!latest.current.work) return;
    nativeBusyRef.current = true;
    setNativeBusy(true);
    setRefreshKey((value) => value + 1);
  };
  useEffect(() => {
    if (closeAfterSettlement.current && !busyRef.current && !nativeBusy) {
      closeAfterSettlement.current = false;
      latest.current.onClose();
    }
  }, [phase, nativeBusy, closePending]);
  const valid = (id: number, generation: number) =>
    mounted.current &&
    attempt.current === id &&
    latest.current.generation === generation;
  const expectedSession = () => {
    const value = latest.current.snapshot();
    return value === undefined ? undefined : structuredClone(value);
  };
  const release = (token: string) =>
    latest.current.commands
      .cancel(token)
      .catch((reason) =>
        console.warn("Profile preparation release failed", reason),
      );
  const prepare = async (retry = false) => {
    if (
      retry &&
      (busyRef.current || nativeBusyRef.current || confirmation.current)
    )
      return;
    const id = ++attempt.current,
      generation = latest.current.generation;
    transition(retry ? "retrying" : "preparing");
    setError("");
    setApproved(false);
    setDiscard(false);
    setExported("");
    let failure = "";
    try {
      if (retry) await latest.current.retry();
      try {
        await latest.current.flush();
      } catch (reason) {
        failure = String(reason);
      }
      if (!valid(id, generation)) return;
      setSaveFailure(failure);
      // FIX 2: Refresh the already granted candidate after saving. A new token
      // needs new review, and exports derive native state from that exact token.
      const result =
        retry && pendingToken.current
          ? await latest.current.commands.refresh(
              pendingToken.current,
              expectedSession(),
            )
          : await latest.current.commands.prepare(
              latest.current.mode,
              expectedSession(),
            );
      if (!valid(id, generation)) {
        if (result) await release(result.token);
        return;
      }
      if (!result) {
        closeAfterSettlement.current = true;
        setClosePending(true);
        transition("failed");
        return;
      }
      pendingToken.current = result.token;
      setPreview(result);
      transition("preview");
    } catch (reason) {
      if (valid(id, generation)) {
        setError(String(reason));
        if (retry) setSaveFailure(String(reason));
        transition(preview ? "preview" : "failed");
      }
    } finally {
      if (valid(id, generation)) observeSettlement();
    }
  };
  useEffect(() => {
    mounted.current = true;
    if (!initial.current) initial.current = prepare();
    return () => {
      mounted.current = false;
    };
  }, []);
  const dirty =
    !!saveFailure ||
    !!preview?.requiresDisposition ||
    !!preview?.hasPendingSave ||
    !!preview?.rendererChangesUnadmitted;
  const cancel = async () => {
    if (
      busyRef.current ||
      nativeBusyRef.current ||
      confirmation.current ||
      committed.current
    )
      return;
    const id = ++attempt.current,
      generation = latest.current.generation;
    transition("cancelling");
    setError("");
    try {
      if (pendingToken.current)
        await latest.current.commands.cancel(pendingToken.current);
      if (valid(id, generation)) {
        pendingToken.current = null;
        closeAfterSettlement.current = true;
        setClosePending(true);
        transition("failed");
      }
    } catch (reason) {
      if (valid(id, generation)) {
        setError(String(reason));
        transition(preview ? "preview" : "failed");
      }
    } finally {
      if (valid(id, generation)) observeSettlement();
    }
  };
  // FIX 3: Only explicit native precommit failures permit a different decision.
  // Unknown/lost responses retain the exact token and disposition for replay.
  const confirm = async (
    token: string,
    disposition: ProfileDisposition,
    id: number,
    generation: number,
  ) => {
    confirmation.current = { token, disposition };
    transition("switching");
    try {
      const status = await latest.current.commands.confirm(token, disposition);
      if (!valid(id, generation)) return;
      committed.current = true;
      pendingToken.current = null;
      transition("reloading");
      await latest.current.onSwitched(status);
    } catch (reason) {
      if (!valid(id, generation)) return;
      setError(
        committed.current
          ? `The profile was switched, but its view could not reload: ${String(reason)}. Reload the window to open the selected profile.`
          : String(reason),
      );
      if (committed.current) {
        transition("reloading");
        return;
      }
      const code = /^(?:Error:\s*)?([A-Z][A-Z0-9_]+)(?::|$)/.exec(
        String(reason),
      )?.[1];
      if (
        code &&
        [
          "PROFILE_CURRENT_UI_UNADMITTED",
          "PROFILE_SAVE_REQUIRED",
          "PROFILE_COPY_REQUIRED",
          "STALE_PROFILE_PREPARATION",
          "PROFILE_PREPARATION_EXPIRED",
          "RECOVERY_CANCELLED",
        ].includes(code)
      ) {
        confirmation.current = null;
        transition("preview");
      } else transition("uncertain");
    }
  };
  const switchTo = async (
    disposition: ProfileDisposition,
    makeCopy = false,
  ) => {
    if (
      busyRef.current ||
      nativeBusyRef.current ||
      confirmation.current ||
      !preview ||
      !approved ||
      committed.current ||
      (disposition === "discard" && !discard) ||
      (disposition === "saved" && dirty) ||
      (disposition === "recovery-copy" &&
        (preview.rendererChangesUnadmitted || (!makeCopy && !exported)))
    )
      return;
    const id = ++attempt.current,
      generation = latest.current.generation;
    setError("");
    try {
      if (makeCopy) {
        transition("exporting");
        const output = await latest.current.commands.exportCopy(preview.token);
        if (!valid(id, generation)) return;
        if (!output) {
          setError(
            "Recovery copy cancelled. Your pending work remains in memory and the current profile stays active.",
          );
          transition("preview");
          return;
        }
        setExported(output);
      }
      if (valid(id, generation))
        await confirm(preview.token, disposition, id, generation);
    } catch (reason) {
      if (valid(id, generation)) {
        setError(String(reason));
        transition("preview");
      }
    } finally {
      if (valid(id, generation) && !committed.current) observeSettlement();
    }
  };
  const retryExact = async () => {
    if (busyRef.current || nativeBusyRef.current || !confirmation.current)
      return;
    const id = ++attempt.current,
      generation = latest.current.generation;
    const fixed = confirmation.current;
    setError("");
    await confirm(fixed.token, fixed.disposition, id, generation);
    if (valid(id, generation) && !committed.current) observeSettlement();
  };
  const busy = busyRef.current || nativeBusy || phase === "uncertain";
  return (
    <Modal
      title={
        props.mode === "create"
          ? "Choose recovery storage"
          : "Open existing profile"
      }
      wide
      onClose={() => void cancel()}
      closeDisabled={busy}
      footer={
        <button disabled={busy} onClick={() => void cancel()}>
          Keep current profile
        </button>
      }
    >
      {props.onHelp && (
        <button onClick={props.onHelp}>Help and diagnostics</button>
      )}
      {props.work && (
        <RecoveryWorkControls
          actions={props.work}
          watch={busyRef.current}
          refreshKey={refreshKey}
          onActiveChange={workChanged}
        />
      )}
      <p>
        Your ranking session, file journals and recovery backups belong to a
        profile. Selecting another profile preserves the current profile's
        stored records.
      </p>
      {props.current && (
        <section aria-label="Current profile">
          <h3>Current profile</h3>
          <p>
            {props.current.isDefault
              ? "Default storage"
              : "Selected recovery storage"}{" "}
            · {props.current.id || "Identity unavailable"}
          </p>
          <p className="muted">{props.current.displayPath}</p>
        </section>
      )}
      {busy && (
        <p role="status" className="notice">
          <LoaderCircle className="spin" size={17} />{" "}
          {phase === "preparing"
            ? "Waiting for queued saves, then preparing the folder you choose…"
            : phase === "retrying"
              ? "Retrying the pending session save…"
              : phase === "exporting"
                ? "Choose a recovery-copy destination. Cancelling keeps your pending work here."
                : phase === "cancelling"
                  ? "Releasing the prepared profile…"
                  : phase === "reloading"
                    ? "Selected profile committed. Reloading its workspace…"
                    : "Revalidating the selected profile and your save disposition…"}
        </p>
      )}
      {phase === "uncertain" && (
        <div className="notice">
          <p>
            The profile-selection outcome is uncertain. Keep the exact
            confirmation and decision until native storage resolves this
            request.
          </p>
          <button
            disabled={nativeBusy || busyRef.current}
            onClick={() => void retryExact()}
          >
            Retry exact profile confirmation
          </button>
        </div>
      )}
      {phase === "reloading" && props.onReload && (
        <button
          className="primary"
          onClick={() => {
            try {
              props.onReload?.();
            } catch (reason) {
              setError(
                `The selected profile is committed. Reloading still failed: ${String(reason)}`,
              );
            }
          }}
        >
          Reload selected profile
        </button>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {phase === "failed" && (
        <>
          <p>
            The selected storage could not be prepared. Your current profile and
            pending work remain unchanged.
          </p>
          <button disabled={busy} onClick={() => void prepare()}>
            <FolderOpen size={16} /> Choose another folder
          </button>
        </>
      )}
      {preview && (
        <section aria-label="Prepared profile preview">
          <h3>
            {preview.mode === "create"
              ? "New recovery storage"
              : "Existing profile"}
          </h3>
          <p>
            <strong>{preview.profile.displayPath}</strong>
          </p>
          <p className="muted">
            Profile identity:{" "}
            {preview.profile.id ||
              "Assigned when the prepared profile is confirmed"}{" "}
            · {preview.profile.writable ? "Writable" : "Read-only recovery"}
          </p>
          {!!preview.pendingOperations && (
            <p className="notice">
              {preview.pendingOperations} file operations remain pending. They
              stay with their original profile and require its trusted journals
              to recover.
            </p>
          )}
          {!!preview.warnings.length && (
            <ul>
              {preview.warnings.map((warning, index) => (
                <li key={index}>{warning}</li>
              ))}
            </ul>
          )}
          <p>
            Only the folder selected in the native picker is authorized. Image
            files and recovery backups are not copied to the selected profile.
          </p>
          <label className="check-label">
            <input
              type="checkbox"
              checked={approved}
              disabled={busy}
              onChange={(event) => setApproved(event.target.checked)}
            />{" "}
            I reviewed this profile and its location.
          </label>
          {dirty && (
            <div className="notice">
              <h3>Pending work needs a decision</h3>
              <p>
                The current session has changes without a durable
                acknowledgment. Keep this dialog open while retrying or saving a
                verified recovery copy, or explicitly discard those unsaved
                edits.
              </p>
              {saveFailure && <p>{saveFailure}</p>}
            </div>
          )}
          {preview.rendererChangesUnadmitted && (
            <p className="notice">
              This window has newer changes than native storage. Retry saving
              and review this same profile again, or explicitly discard these
              unsaved edits. A native-only recovery copy cannot include them.
            </p>
          )}
          {exported && (
            <p className="notice">
              Recovery copy saved: {exported}. Native confirmation checks that
              this copy still matches the pending state.
            </p>
          )}
          <div className="button-row">
            {!dirty && (
              <button
                className="primary"
                disabled={busy || !approved}
                onClick={() => void switchTo("saved")}
              >
                Switch profile
              </button>
            )}
            {dirty && (
              <>
                <button
                  className="primary"
                  disabled={busy || props.canRetrySave === false}
                  onClick={() => void prepare(true)}
                >
                  Retry save and review profile
                </button>
                <button
                  disabled={
                    busy || !approved || preview.rendererChangesUnadmitted
                  }
                  onClick={() => void switchTo("recovery-copy", true)}
                >
                  Save recovery copy and switch
                </button>
                {exported && (
                  <button
                    disabled={busy || !approved}
                    onClick={() => void switchTo("recovery-copy")}
                  >
                    Switch using saved recovery copy
                  </button>
                )}
              </>
            )}
          </div>
          {dirty && (
            <div className="close-discard">
              <label className="check-label">
                <input
                  type="checkbox"
                  checked={discard}
                  disabled={busy}
                  onChange={(event) => setDiscard(event.target.checked)}
                />{" "}
                Discard only the current unsaved edits. Keep the current
                profile's saved history and files.
              </label>
              <button
                disabled={busy || !approved || !discard}
                onClick={() => void switchTo("discard")}
              >
                Switch and discard pending edits
              </button>
            </div>
          )}
        </section>
      )}
    </Modal>
  );
}
