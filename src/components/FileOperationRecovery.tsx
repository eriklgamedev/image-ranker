import { useEffect, useRef, useState } from "react";
import { api, type BootStatus, type OperationRestriction } from "../native";
import { Modal } from "./Modal";
import {
  OperationRestartReview,
  type OperationRestartCommands,
} from "./OperationRestartReview";

const restartCommands: OperationRestartCommands = {
  prepare: (expected) => api.prepareOperationRestart(expected),
  exportCopy: (token) => api.exportOperationRestart(token),
  confirm: (token, expected) => api.confirmOperationRestart(token, expected),
  cancel: (token) => api.cancelOperationRestart(token),
};

export function operationRestrictionFromBoot(
  status: Pick<BootStatus, "issues">,
): OperationRestriction | null {
  const restart = status.issues.find(
    (issue) => issue.code === "FILE_OPERATION_RESTART_REQUIRED",
  );
  if (restart) return { kind: "restart", message: restart.message };
  const publication = status.issues.find(
    (issue) => issue.code === "FILE_OPERATION_PUBLICATION_REQUIRED",
  );
  return publication
    ? { kind: "publication", message: publication.message }
    : null;
}

export function FileOperationRecovery({
  restriction,
  onRetry,
  onHelp,
  flush,
  snapshot,
  generation,
}: {
  restriction: OperationRestriction;
  onRetry: () => Promise<void>;
  onHelp: () => void;
  flush: () => Promise<void>;
  snapshot: () => unknown | undefined;
  generation: number;
}) {
  const [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const running = useRef(false),
    alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const retry = async () => {
    if (running.current || restriction.kind !== "publication") return;
    running.current = true;
    setBusy(true);
    setError("");
    try {
      await onRetry();
    } catch (reason) {
      if (alive.current) setError(String(reason));
    } finally {
      running.current = false;
      if (alive.current) setBusy(false);
    }
  };
  return (
    <Modal
      title="File operation needs recovery"
      onClose={() => {}}
      closeDisabled
    >
      <p role="alert">{restriction.message}</p>
      {restriction.kind === "restart" ? (
        <>
          <p>
            The file worker stopped without a verified outcome. Restart Image
            Ranker so native recovery can inspect its retained journal.
            Reloading this window does not restart the application.
          </p>
          <OperationRestartReview
            key={generation}
            generation={generation}
            commands={restartCommands}
            flush={flush}
            snapshot={snapshot}
          />
        </>
      ) : (
        <>
          <p>
            The native file worker has finished, but its result has not been
            published into this workspace. Ranking, profile changes, and normal
            close remain blocked.
          </p>
          <p>
            Retry publishes the retained result. It does not run the file
            operation again.
          </p>
          <button className="primary" disabled={busy} onClick={retry}>
            {busy
              ? "Publishing retained result…"
              : "Retry file result publication"}
          </button>
        </>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <button onClick={onHelp}>Help and local diagnostics</button>
    </Modal>
  );
}
