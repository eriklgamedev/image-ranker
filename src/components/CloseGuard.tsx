import { useEffect, useRef, useState } from "react";
import { LoaderCircle } from "lucide-react";
import { api, desktop } from "../native";
import { Modal } from "./Modal";

type Phase =
  "waiting" | "blocked" | "retrying" | "exporting" | "stopping" | "closing";
export function CloseGuard(props: {
  freeze: (frozen: boolean) => void;
  flush: () => Promise<void>;
  retry: () => Promise<void>;
  snapshot: () => unknown;
  activeWork: () => boolean;
}) {
  const latest = useRef(props);
  latest.current = props;
  const tokenRef = useRef<string | null>(null);
  const [token, setToken] = useState<string | null>(null);
  const [phase, setPhase] = useState<Phase>("waiting");
  const [error, setError] = useState("");
  const [exported, setExported] = useState("");
  const [discard, setDiscard] = useState(false);
  const live = useRef(true);
  const valid = (expected: string) =>
    live.current && tokenRef.current === expected;
  const attempt = async (expected: string, retry = false) => {
    setPhase(retry ? "retrying" : "waiting");
    setError("");
    try {
      if (latest.current.activeWork())
        throw new Error(
          "Active file work has not reached a saved boundary. Stop it safely, then retry closing.",
        );
      if (retry) await latest.current.retry();
      await latest.current.flush();
      if (!valid(expected)) return;
      if (latest.current.activeWork())
        throw new Error(
          "Active file work has not reached a saved boundary. Stop it safely, then retry closing.",
        );
      setPhase("closing");
      await api.confirmClose(expected, "saved");
    } catch (reason) {
      if (valid(expected)) {
        setError(String(reason));
        setPhase("blocked");
      }
    }
  };
  useEffect(() => {
    if (!desktop) return;
    live.current = true;
    let active = true;
    let detach: (() => void) | undefined;
    api
      .subscribeCloseRequested((request) => {
        if (!active || !live.current || tokenRef.current === request.token)
          return;
        tokenRef.current = request.token;
        latest.current.freeze(true);
        setToken(request.token);
        setExported("");
        setDiscard(false);
        void attempt(request.token);
      })
      .then((unlisten) => {
        if (active) detach = unlisten;
        else unlisten();
      })
      .catch((reason) => {
        // A missing listener must be visible: native keeps close blocked safely.
        if (active)
          setError(`Close protection could not start: ${String(reason)}`);
      });
    return () => {
      active = false;
      live.current = false;
      detach?.();
    };
  }, []);
  const cancel = async () => {
    if (!token || !["blocked", "waiting", "retrying"].includes(phase)) return;
    try {
      await api.cancelClose(token);
      tokenRef.current = null;
      latest.current.freeze(false);
      setToken(null);
      setError("");
    } catch (reason) {
      setError(String(reason));
    }
  };
  const closeWith = async (disposition: "discard" | "recovery-copy") => {
    if (!token || (disposition === "discard" && !discard)) return;
    setError("");
    setPhase("closing");
    try {
      if (latest.current.activeWork())
        throw new Error(
          "Active file work must stop at a saved boundary before closing.",
        );
      await api.confirmClose(token, disposition);
    } catch (reason) {
      if (valid(token)) {
        setError(String(reason));
        setPhase("blocked");
      }
    }
  };
  const exportAndClose = async () => {
    if (!token) return;
    setPhase("exporting");
    setError("");
    try {
      if (latest.current.activeWork())
        throw new Error(
          "Stop active file work before capturing a recovery copy.",
        );
      const path = await api.exportSessionCopy(
        structuredClone(latest.current.snapshot()),
      );
      if (!valid(token)) return;
      if (!path) {
        setError(
          "Recovery copy cancelled. The app remains open and your pending work stays in memory.",
        );
        setPhase("blocked");
        return;
      }
      setExported(path);
      await closeWith("recovery-copy");
    } catch (reason) {
      if (valid(token)) {
        setError(String(reason));
        setPhase("blocked");
      }
    }
  };
  if (!token)
    return error ? (
      <p className="error close-listener-error" role="alert">
        {error}
      </p>
    ) : null;
  const busy = phase !== "blocked";
  const canStayOpen = ["blocked", "waiting", "retrying"].includes(phase);
  return (
    <Modal
      title="Close Image Ranker"
      closeDisabled={!canStayOpen}
      onClose={() => void cancel()}
      footer={
        <button disabled={!canStayOpen} onClick={() => void cancel()}>
          Stay open
        </button>
      }
    >
      {busy ? (
        <p role="status" className="notice">
          <LoaderCircle className="spin" size={18} />
          {phase === "exporting"
            ? "Choose a recovery-copy destination. The app stays open if you cancel."
            : phase === "stopping"
              ? "Requesting a safe pause for active work…"
              : phase === "closing"
                ? "Checking the final saved boundary before closing…"
                : "Waiting for queued changes to receive a durable save acknowledgment…"}
        </p>
      ) : (
        <p>
          Your latest work remains visible in memory. Resolve the save or choose
          how to preserve it before closing.
        </p>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {exported && (
        <p className="notice">
          Session recovery copy saved: {exported}. This preserves session
          history and pending work; it does not include image files or overwrite
          backups.
        </p>
      )}
      {!busy && (
        <>
          <div className="button-row">
            <button
              className="primary"
              onClick={() => void attempt(token, true)}
            >
              Retry save and close
            </button>
            <button onClick={() => void exportAndClose()}>
              Save recovery copy and close
            </button>
            {exported && (
              <button onClick={() => void closeWith("recovery-copy")}>
                Close using saved recovery copy
              </button>
            )}
          </div>
          {(latest.current.activeWork() ||
            /CLOSE_BUSY|active|boundary/i.test(error)) && (
            <div className="close-job-actions">
              <p>
                Active file operations stop at a journaled boundary and can be
                recovered when the app reopens.
              </p>
              <button
                onClick={async () => {
                  setPhase("stopping");
                  try {
                    await Promise.all([
                      api.cancelScan(),
                      api.cancelOperation(),
                    ]);
                    setError(
                      "Stop requested. Retry closing after the active work has reached a safe boundary.",
                    );
                  } catch (reason) {
                    setError(String(reason));
                  } finally {
                    setPhase("blocked");
                  }
                }}
              >
                Stop active work safely
              </button>
            </div>
          )}
          <div className="close-discard">
            <label className="check-label">
              <input
                type="checkbox"
                checked={discard}
                onChange={(event) => setDiscard(event.target.checked)}
              />
              I accept losing changes without a saved acknowledgment.
            </label>
            <button
              className="danger"
              disabled={!discard}
              onClick={() => void closeWith("discard")}
            >
              Discard unsaved changes and close
            </button>
            <p className="muted">
              Previously saved records and native recovery journals are
              preserved.
            </p>
          </div>
        </>
      )}
    </Modal>
  );
}
