import { useEffect, useRef, useState } from "react";
import type { OperationRestartPreview } from "../native";

export interface OperationRestartCommands {
  prepare: (
    expectedCurrentSession?: unknown,
  ) => Promise<OperationRestartPreview>;
  exportCopy: (token: string) => Promise<OperationRestartPreview | null>;
  confirm: (token: string, expectedCurrentSession?: unknown) => Promise<void>;
  cancel: (token: string) => Promise<void>;
}
type Phase =
  | "idle"
  | "preparing"
  | "review"
  | "exporting"
  | "confirming"
  | "cancelling"
  | "close-uncertain"
  | "close-requested";
const precommit = new Set([
  "OPERATION_RESTART_UNAVAILABLE",
  "OPERATION_RESTART_STALE",
  "OPERATION_RESTART_BUSY",
  "OPERATION_RESTART_COPY_REQUIRED",
  "OPERATION_RESTART_EXPECTATION_CHANGED",
  "CAPACITY_LIMIT",
  "RECOVERY_CORRUPT",
]);
function code(reason: unknown) {
  return /^([A-Z_]+)(?::|$)/.exec(
    String(reason).replace(/^Error:\s*/, ""),
  )?.[1];
}
function validated(value: OperationRestartPreview): OperationRestartPreview {
  if (
    !value ||
    typeof value.token !== "string" ||
    !value.token.trim() ||
    typeof value.requestId !== "string" ||
    !value.requestId.trim() ||
    !Number.isSafeInteger(value.savedRevision) ||
    value.savedRevision < 0 ||
    !Number.isSafeInteger(value.pendingActions) ||
    value.pendingActions < 0 ||
    typeof value.rendererProposal !== "boolean" ||
    typeof value.copyReady !== "boolean" ||
    (value.copyPath !== null && typeof value.copyPath !== "string") ||
    (value.copyReady && !value.copyPath?.trim()) ||
    !Array.isArray(value.warnings) ||
    value.warnings.some((warning) => typeof warning !== "string")
  ) {
    throw new Error(
      "The native restart review response is incomplete. Review again before closing.",
    );
  }
  return value;
}

/** App's native restriction remains in force through every state, including Cancel. */
export function OperationRestartReview(props: {
  commands: OperationRestartCommands;
  flush: () => Promise<void>;
  snapshot: () => unknown | undefined;
  generation: number;
}) {
  const latest = useRef(props);
  latest.current = props;
  const alive = useRef(false),
    running = useRef(false),
    attempt = useRef(0);
  const held = useRef<OperationRestartPreview | null>(null);
  const expected = useRef<unknown | undefined>(undefined);
  const [preview, setPreview] = useState<OperationRestartPreview | null>(null);
  const [phase, setPhase] = useState<Phase>("idle"),
    [error, setError] = useState("");
  const [saveFailure, setSaveFailure] = useState(""),
    [notice, setNotice] = useState("");
  const [needsReview, setNeedsReview] = useState(false);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      attempt.current++;
    };
  }, []);
  const valid = (id: number, generation: number) =>
    alive.current &&
    id === attempt.current &&
    generation === latest.current.generation;
  const begin = (next: Phase) => {
    if (running.current) return null;
    running.current = true;
    const id = ++attempt.current;
    setPhase(next);
    setError("");
    setNotice("");
    return { id, generation: latest.current.generation };
  };
  const prepare = async () => {
    if (phase === "close-uncertain" || phase === "close-requested") return;
    const action = begin("preparing");
    if (!action) return;
    try {
      let failed = "";
      try {
        await latest.current.flush();
      } catch (reason) {
        failed = String(reason);
      }
      if (!valid(action.id, action.generation)) return;
      setSaveFailure(failed);
      const value = latest.current.snapshot();
      const frozen = value === undefined ? undefined : structuredClone(value);
      const response = validated(await latest.current.commands.prepare(frozen));
      if (!valid(action.id, action.generation)) return;
      held.current = response;
      expected.current = frozen;
      setPreview(response);
      setNeedsReview(false);
      setPhase("review");
    } catch (reason) {
      if (valid(action.id, action.generation)) {
        setError(String(reason));
        setNeedsReview(true);
        setPhase(held.current ? "review" : "idle");
      }
    } finally {
      running.current = false;
    }
  };
  const exportCopy = async () => {
    const current = held.current;
    if (
      !current ||
      needsReview ||
      phase === "close-uncertain" ||
      phase === "close-requested"
    )
      return;
    const action = begin("exporting");
    if (!action) return;
    try {
      const response = await latest.current.commands.exportCopy(current.token);
      if (!valid(action.id, action.generation)) return;
      if (response === null) {
        setNotice(
          "The file picker was cancelled. No new recovery copy was confirmed.",
        );
        setPhase("review");
        return;
      }
      const next = validated(response);
      if (
        next.token !== current.token ||
        next.requestId !== current.requestId ||
        next.savedRevision !== current.savedRevision ||
        next.pendingActions !== current.pendingActions ||
        next.rendererProposal !== current.rendererProposal
      )
        throw new Error(
          "The copy response belongs to a different restart review. This copy has not authorized closing.",
        );
      held.current = next;
      setPreview(next);
      setPhase("review");
      setNotice(
        next.copyReady
          ? "Native recovery-copy verification is complete for this restart review."
          : "The recovery copy has not received native verification. Keep this review open.",
      );
    } catch (reason) {
      if (valid(action.id, action.generation)) {
        setError(String(reason));
        setPhase("review");
        if (
          [
            "OPERATION_RESTART_STALE",
            "OPERATION_RESTART_EXPECTATION_CHANGED",
          ].includes(code(reason) ?? "")
        )
          setNeedsReview(true);
      }
    } finally {
      running.current = false;
    }
  };
  const confirm = async () => {
    const current = held.current;
    if (!current?.copyReady || needsReview) return;
    const action = begin("confirming");
    if (!action) return;
    try {
      // Exact prepared expectation is retained after an uncertain response. A
      // later mutable snapshot must never authorize closing a different layer.
      await latest.current.commands.confirm(current.token, expected.current);
      if (!valid(action.id, action.generation)) return;
      setPhase("close-requested");
      setNotice(
        "Native close has been requested. If this window remains open, retry this same close request. Open Image Ranker again to reconcile the file journal.",
      );
    } catch (reason) {
      if (valid(action.id, action.generation)) {
        const nativeCode = code(reason);
        setError(String(reason));
        if (nativeCode && precommit.has(nativeCode)) {
          setPhase("review");
          if (
            [
              "OPERATION_RESTART_STALE",
              "OPERATION_RESTART_EXPECTATION_CHANGED",
            ].includes(nativeCode)
          )
            setNeedsReview(true);
          if (nativeCode === "OPERATION_RESTART_COPY_REQUIRED") {
            const rejected = { ...current, copyReady: false };
            held.current = rejected;
            setPreview(rejected);
          }
        } else setPhase("close-uncertain");
      }
    } finally {
      running.current = false;
    }
  };
  const cancel = async () => {
    if (phase === "close-uncertain" || phase === "close-requested") return;
    const current = held.current;
    if (!current) return;
    const action = begin("cancelling");
    if (!action) return;
    try {
      await latest.current.commands.cancel(current.token);
      if (!valid(action.id, action.generation)) return;
      held.current = null;
      expected.current = undefined;
      setPreview(null);
      setNeedsReview(false);
      setSaveFailure("");
      setPhase("idle");
      setNotice(
        "Restart review cancelled. Current work remains retained and file editing stays locked.",
      );
    } catch (reason) {
      if (valid(action.id, action.generation)) {
        setError(String(reason));
        setPhase("review");
      }
    } finally {
      running.current = false;
    }
  };
  const busy = ["preparing", "exporting", "confirming", "cancelling"].includes(
    phase,
  );
  const exactClose = ["close-uncertain", "close-requested"].includes(phase);
  return (
    <section aria-label="Preserve work before restart" aria-busy={busy}>
      <h3>Preserve work before restart</h3>
      <p>
        The file-operation outcome is unverified. A recovery copy preserves the
        verified saved session, retained pending work, and any reviewed renderer
        changes; it does not confirm, undo, or repeat the file actions.
      </p>
      {preview && (
        <>
          <dl className="restart-summary">
            <dt>Verified saved revision</dt>
            <dd>{preview.savedRevision}</dd>
            <dt>Retained pending actions</dt>
            <dd>{preview.pendingActions}</dd>
            <dt>Additional renderer changes</dt>
            <dd>
              {preview.rendererProposal
                ? "Included in this review"
                : "No additional proposal reported by native recovery"}
            </dd>
            <dt>Recovery copy</dt>
            <dd>
              {preview.copyReady && !needsReview
                ? "Verified for this exact review"
                : "Not verified for closing"}
            </dd>
          </dl>
          {preview.copyPath && (
            <p className="break">Recovery copy location: {preview.copyPath}</p>
          )}
          {preview.warnings.length > 0 && (
            <ul>
              {preview.warnings.map((warning, index) => (
                <li key={index}>{warning}</li>
              ))}
            </ul>
          )}
        </>
      )}
      {saveFailure && (
        <p className="muted">
          The save queue did not finish durably: {saveFailure}. This review
          preserves available work without treating the failed save as
          acknowledged.
        </p>
      )}
      {needsReview && (
        <p role="status">
          Review the retained work again before exporting or closing. A new
          review requires its own verified copy.
        </p>
      )}
      {phase === "close-uncertain" && (
        <p role="alert">
          The close outcome is unconfirmed. Keep this exact review and retry the
          same close request; no new approval or session replacement has been
          inferred.
        </p>
      )}
      {notice && <p role="status">{notice}</p>}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <div className="restart-actions">
        {!preview && (
          <button className="primary" disabled={busy} onClick={prepare}>
            {phase === "preparing"
              ? "Preparing restart review…"
              : "Preserve work and review restart"}
          </button>
        )}
        {preview && (
          <>
            {needsReview && (
              <button
                className="primary"
                disabled={busy || exactClose}
                onClick={prepare}
              >
                Review retained work again
              </button>
            )}
            <button
              disabled={busy || needsReview || exactClose}
              onClick={exportCopy}
            >
              {phase === "exporting"
                ? "Saving recovery copy…"
                : "Save restart recovery copy"}
            </button>
            <button
              className="primary"
              disabled={busy || needsReview || !preview.copyReady}
              onClick={confirm}
            >
              {phase === "confirming"
                ? "Requesting safe close…"
                : exactClose
                  ? "Retry close for restart"
                  : "Close for restart"}
            </button>
            <button disabled={busy || exactClose} onClick={cancel}>
              {phase === "cancelling"
                ? "Cancelling restart review…"
                : "Cancel restart review"}
            </button>
          </>
        )}
      </div>
      <p className="muted">
        Normal close remains blocked. Restarting this application is separate
        from reloading its window.
      </p>
    </section>
  );
}
