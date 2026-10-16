import { useCallback, useEffect, useRef, useState } from "react";
import { api, desktop, type ImageWorkStatus } from "../native";
import { bytes } from "./OperationDialog";

// FIX 1: Native recovery can repair worker ownership or a temporary output.
// Neither action proves all derived-output owners have released their guards.
function recoveryMessage(
  action: "reap" | "retry-output",
  result: ImageWorkStatus["decoder"],
) {
  if (result.recoveryAction === "retry-output-cleanup")
    return "Temporary output cleanup remains unconfirmed. The output and its reservation remain retained.";
  if (result.unavailable)
    return action === "retry-output"
      ? "Temporary output cleanup remains unconfirmed. Native ownership and reservations remain retained."
      : "Worker exit remains unverified. Its memory reservation is retained.";
  if (result.active || result.pending || result.reservedBytes)
    return "Recovery request finished. Native decoder work or memory reservations remain active. Refresh to confirm their release.";
  if (result.liveOutputs || result.outputReservedBytes) {
    const outputs = result.liveOutputs
      ? `${result.liveOutputs} derived output${result.liveOutputs === 1 ? " remains" : "s remain"} retained`
      : "Derived output reservations remain retained";
    return `${action === "retry-output" ? "Temporary output cleanup retry" : "Native worker cleanup"} finished. ${outputs} (${bytes(result.outputReservedBytes)} reserved). Native owners, including protection checks, can retain outputs until they are released.`;
  }
  return action === "retry-output"
    ? "Temporary output cleanup completed. Image work can be retried."
    : "Native worker cleanup completed. Image work can be retried.";
}

/** Native control commands remain usable while an image decoder owns no Backend lock. */
export function ImageWorkSettings() {
  const [status, setStatus] = useState<ImageWorkStatus | null>(null);
  const [error, setError] = useState("");
  const [statusError, setStatusError] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const alive = useRef(false),
    sequence = useRef(0),
    actionGeneration = useRef(0),
    actionBusy = useRef(false);
  const refresh = useCallback(async (force = false) => {
    if (actionBusy.current && !force) return;
    const request = ++sequence.current;
    try {
      const next = await api.imageWorkStatus();
      if (alive.current && request === sequence.current) {
        setStatus(next);
        setStatusError("");
      }
    } catch (reason) {
      if (alive.current && request === sequence.current)
        setStatusError(String(reason));
    }
  }, []);
  useEffect(() => {
    if (!desktop) return;
    alive.current = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let stopped = false;
    const poll = async () => {
      await refresh();
      if (!stopped) timer = setTimeout(() => void poll(), 1500);
    };
    void poll();
    return () => {
      stopped = true;
      alive.current = false;
      sequence.current++;
      actionGeneration.current++;
      clearTimeout(timer);
    };
  }, [refresh]);
  const act = async (
    action: "cancel" | "reap" | "retry-output" | "retry-cache",
    requestId?: string,
  ) => {
    if (actionBusy.current) return;
    actionBusy.current = true;
    const generation = ++actionGeneration.current;
    const current = () =>
      alive.current && generation === actionGeneration.current;
    setBusy(true);
    setError("");
    setMessage("");
    sequence.current++;
    try {
      if (action === "cancel") {
        const count = await api.cancelImageWork();
        if (current())
          setMessage(
            count
              ? `Cancellation requested for ${count} image request${count === 1 ? "" : "s"}. Work remains reserved until it exits.`
              : "No image or cache maintenance requests remain to cancel.",
          );
      } else if (action === "reap" || action === "retry-output") {
        const result = await api.reapImageWorker();
        if (current()) setMessage(recoveryMessage(action, result));
      } else if (requestId) {
        const result = await api.retryCacheMaintenance(requestId);
        if (current()) {
          setStatus((previous) =>
            previous ? { ...previous, cacheMaintenance: result } : previous,
          );
          setMessage(
            "Cache maintenance retry requested. Cleanup remains reserved until native status confirms retirement.",
          );
        }
      }
      if (current()) await refresh(true);
    } catch (reason) {
      if (current()) setError(String(reason));
    } finally {
      if (generation === actionGeneration.current) {
        actionBusy.current = false;
        if (alive.current) setBusy(false);
      }
    }
  };
  if (!desktop) return null;
  const decoder = status?.decoder;
  return (
    <section
      className="support-section image-work-settings"
      aria-label="Native image work"
    >
      <h3>Native image work</h3>
      <p className="muted">
        Image decoding runs in a separate native worker. Status and cancellation
        remain available while previews or folders are being processed.
      </p>
      {!status && <p>Reading native image work…</p>}
      {status && (
        <>
          <p>
            {status.active} active image requests · {status.limit} request limit
          </p>
          {!!status.jobs.length && (
            <ul aria-label="Active image requests">
              {status.jobs.map((job) => (
                <li key={job.requestId}>
                  {job.kind === "worker-recovery"
                    ? "Worker cleanup"
                    : job.kind === "file-mutation"
                      ? "File changes · preview admission paused"
                      : job.kind[0].toUpperCase() + job.kind.slice(1)}{" "}
                  · {job.phase.replaceAll("-", " ")}
                  {job.cancelled
                    ? " · Cancellation requested; waiting for exit"
                    : ""}
                </li>
              ))}
            </ul>
          )}
          {!!status.cacheMaintenance?.owners.length && (
            <div aria-label="Cache maintenance work">
              <p>
                Cache maintenance retains at most{" "}
                {status.cacheMaintenance.ownerLimit} workspace caches until
                their previews and cleanup are released.
              </p>
              <ul>
                {status.cacheMaintenance.owners.map((owner) => (
                  <li
                    key={owner.requestId}
                    aria-label={`Cache maintenance ${owner.requestId}`}
                  >
                    {owner.phase.replaceAll("-", " ")}
                    {owner.cancelled ? " · Cancellation requested" : ""}
                    {owner.lastError && (
                      <p className="error">{owner.lastError}</p>
                    )}
                    {!owner.runnable &&
                      !owner.retirementComplete &&
                      !owner.cancelled &&
                      owner.phase !== "retry-required" && (
                        <p className="muted">
                          Waiting for displayed previews or retained resources
                          to be released.
                        </p>
                      )}
                    {(owner.cancelled || owner.phase === "retry-required") &&
                      !owner.retirementComplete && (
                        <button
                          onClick={() =>
                            void act("retry-cache", owner.requestId)
                          }
                          disabled={busy || owner.usage.maintenanceRunning}
                        >
                          Retry cache maintenance
                        </button>
                      )}
                  </li>
                ))}
              </ul>
            </div>
          )}
          <p className="muted">
            Decoder: {decoder!.active} running, {decoder!.pending} queued ·{" "}
            {bytes(decoder!.reservedBytes)} reserved. Derived outputs:{" "}
            {decoder!.liveOutputs} · {bytes(decoder!.outputReservedBytes)}{" "}
            reserved.
            {decoder!.stage ? ` Current stage: ${decoder!.stage}.` : ""}
          </p>
          {decoder!.unavailable && (
            <p className="notice">
              {/* FIX 2: Output disposition failure retains a file owner, not
                  necessarily an unverified worker process. */}
              {decoder!.recoveryAction === "retry-output-cleanup"
                ? "Temporary output cleanup is unconfirmed. The decoder is unavailable; the output and its reservation remain retained until native cleanup succeeds."
                : "The decoder is unavailable. Its resource reservation stays held until native worker exit is verified."}
            </p>
          )}
          {decoder!.recoveryAction === "restart-application" && (
            <p className="notice">
              Worker ownership cannot be recovered in this process. Save or
              export pending work, then close and reopen Image Ranker.
            </p>
          )}
        </>
      )}
      <div className="button-row">
        <button onClick={() => void refresh()} disabled={busy}>
          Refresh image work
        </button>
        <button
          onClick={() => void act("cancel")}
          disabled={
            busy ||
            (!status?.active &&
              !status?.cacheMaintenance?.owners.some(
                (owner) => owner.runnable && !owner.cancelled,
              ))
          }
        >
          Cancel image work
        </button>
        {decoder?.recoveryAction === "retry-reap" && (
          <button
            onClick={() => void act("reap")}
            disabled={busy || !!status?.active}
          >
            Retry native worker cleanup
          </button>
        )}
        {/* FIX 3: Use the same native recovery endpoint and action mutex while
            retaining the exact action the user requested for its result. */}
        {decoder?.recoveryAction === "retry-output-cleanup" && (
          <button
            onClick={() => void act("retry-output")}
            disabled={busy || !!status?.active}
          >
            Retry temporary output cleanup
          </button>
        )}
      </div>
      {message && <p role="status">{message}</p>}
      {statusError && (
        <p className="error" role="alert">
          {statusError}
        </p>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
