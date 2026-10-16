import { useCallback, useEffect, useRef, useState } from "react";
import { Database, LoaderCircle, RefreshCw } from "lucide-react";
import { api, desktop, type CacheClearResult, type CacheInfo } from "../native";
import { bytes } from "./OperationDialog";

const POLL_INTERVAL_MS = 1000;
const MAX_AUTOMATIC_POLLS = 30;
type ClearTracking = {
  attempted: boolean;
  identity: Pick<CacheClearResult, "requestId" | "generation"> | null;
  outcome: CacheClearResult | null;
  confirmed: boolean;
};
const EMPTY_CLEAR: ClearTracking = {
  attempted: false,
  identity: null,
  outcome: null,
  confirmed: false,
};
const terminal = (outcome: CacheClearResult) =>
  !["pending", "running", "waiting-for-pins"].includes(outcome.state);
const sameTerminal = (previous: CacheClearResult, next: CacheClearResult) =>
  previous.state === next.state &&
  previous.retainedFiles === next.retainedFiles &&
  previous.retainedBytes === next.retainedBytes &&
  previous.retainedCountsKnown === next.retainedCountsKnown &&
  previous.retryOf === next.retryOf &&
  previous.reasons.length === next.reasons.length &&
  previous.reasons.every((reason, index) => reason === next.reasons[index]);

export function CacheSettings() {
  const [info, setInfo] = useState<CacheInfo | null>(null),
    [busy, setBusy] = useState(false),
    [reading, setReading] = useState(false),
    [readRevision, setReadRevision] = useState(0),
    [error, setError] = useState(""),
    [clear, setClear] = useState<ClearTracking>(EMPTY_CLEAR),
    [pollingPaused, setPollingPaused] = useState(false);
  const alive = useRef(false),
    generation = useRef(0),
    readInFlight = useRef(false),
    mutationInFlight = useRef(false),
    clearRef = useRef<ClearTracking>(EMPTY_CLEAR),
    polls = useRef(0);

  const trackClear = useCallback((next: ClearTracking) => {
    clearRef.current = next;
    setClear(next);
  }, []);
  const acceptSnapshot = useCallback(
    (
      next: CacheInfo,
      admission?: { previous: ClearTracking; retry: boolean },
    ) => {
      setInfo(next);
      const previous = clearRef.current;
      const outcome = next.clearRequest;
      if (admission || (!previous.attempted && outcome)) {
        const original = admission?.previous;
        const accepted =
          outcome &&
          (!admission ||
            (admission.retry
              ? outcome.requestId !== original?.identity?.requestId &&
                outcome.retryOf === original?.identity?.requestId &&
                outcome.generation === original?.identity?.generation
              : !original?.outcome ||
                !terminal(original.outcome) ||
                outcome.requestId !== original.outcome.requestId));
        trackClear({
          attempted: true,
          identity: accepted
            ? { requestId: outcome.requestId, generation: outcome.generation }
            : null,
          outcome: accepted ? outcome : (original?.outcome ?? null),
          confirmed: !!accepted,
        });
      } else if (previous.attempted) {
        const matches =
          !!outcome &&
          outcome.requestId === previous.identity?.requestId &&
          outcome.generation === previous.identity?.generation &&
          (!previous.outcome ||
            !terminal(previous.outcome) ||
            sameTerminal(previous.outcome, outcome));
        // An expired receipt or a result for another clear never replaces known
        // failure details and never proves this request completed.
        trackClear({
          ...previous,
          outcome: matches ? outcome : previous.outcome,
          confirmed: matches,
        });
      }
    },
    [trackClear],
  );

  const refresh = useCallback(
    async (manual = false) => {
      if (!alive.current || readInFlight.current || mutationInFlight.current)
        return;
      if (manual) {
        polls.current = 0;
        setPollingPaused(false);
        setError("");
      }
      readInFlight.current = true;
      setReading(true);
      const admitted = generation.current;
      try {
        const requestId = clearRef.current.identity?.requestId;
        const next = requestId
          ? await api.cacheInfo(manual, requestId)
          : await api.cacheInfo(manual);
        if (alive.current && admitted === generation.current) {
          acceptSnapshot(next);
          setError("");
        }
      } catch (cause) {
        if (alive.current && admitted === generation.current) {
          setError(String(cause));
          setPollingPaused(true);
        }
      } finally {
        readInFlight.current = false;
        if (alive.current) {
          setReading(false);
          setReadRevision((value) => value + 1);
        }
      }
    },
    [acceptSnapshot],
  );

  useEffect(() => {
    if (!desktop) return;
    alive.current = true;
    void refresh(true);
    return () => {
      alive.current = false;
      generation.current += 1;
    };
  }, [refresh]);

  const maintenanceActive =
    !!info && (info.maintenancePending || info.maintenanceRunning);
  const inventoryKnown = info?.inventoryKnown === true;
  const awaitingClear =
    clear.attempted &&
    (!clear.confirmed || !clear.outcome || !terminal(clear.outcome));
  useEffect(() => {
    if (
      !desktop ||
      busy ||
      reading ||
      error ||
      pollingPaused ||
      (!maintenanceActive && inventoryKnown && !awaitingClear)
    )
      return;
    if (polls.current >= MAX_AUTOMATIC_POLLS) {
      setPollingPaused(true);
      return;
    }
    const timer = setTimeout(() => {
      polls.current += 1;
      void refresh();
    }, POLL_INTERVAL_MS);
    return () => clearTimeout(timer);
  }, [
    busy,
    reading,
    readRevision,
    error,
    pollingPaused,
    maintenanceActive,
    inventoryKnown,
    awaitingClear,
    info,
    refresh,
  ]);

  async function mutate(kind: "clear" | "retry" | "budget", budget?: number) {
    if (!alive.current || mutationInFlight.current) return;
    const previousClear = clearRef.current;
    const retryOf =
      kind === "retry" ? clearRef.current.identity?.requestId : undefined;
    if (kind === "retry" && !retryOf) return;
    mutationInFlight.current = true;
    const admitted = ++generation.current;
    polls.current = 0;
    setBusy(true);
    setError("");
    if (kind !== "budget")
      trackClear({
        ...clearRef.current,
        attempted: true,
        identity: null,
        confirmed: false,
      });
    setPollingPaused(false);
    try {
      const next =
        kind !== "budget"
          ? retryOf
            ? await api.clearCache(retryOf)
            : await api.clearCache()
          : await api.setCacheLimit(budget!);
      if (alive.current && admitted === generation.current) {
        acceptSnapshot(
          next,
          kind !== "budget"
            ? { previous: previousClear, retry: kind === "retry" }
            : undefined,
        );
      }
    } catch (cause) {
      if (alive.current && admitted === generation.current) {
        setError(String(cause));
        setPollingPaused(true);
      }
    } finally {
      mutationInFlight.current = false;
      if (alive.current && admitted === generation.current) setBusy(false);
    }
  }

  if (!desktop) return null;
  const outcome = clear.outcome;
  const clearConfirmed =
    clear.confirmed &&
    outcome?.state === "completed" &&
    outcome.retainedCountsKnown &&
    outcome.retainedFiles === 0 &&
    outcome.retainedBytes === 0 &&
    outcome.reasons.length === 0 &&
    !error;
  const canRetry =
    (clear.confirmed || busy) &&
    outcome &&
    ["failed", "cancelled", "completed-with-retained-items"].includes(
      outcome.state,
    );
  return (
    <section className="cache-settings">
      <h3>
        <Database size={17} /> Preview cache
      </h3>
      <p className="muted">
        {info
          ? inventoryKnown
            ? `${maintenanceActive ? "Latest inventory: " : ""}${bytes(info.sizeBytes)} in ${info.files} files · ${bytes(info.budgetBytes)} budget`
            : `Cache inventory is not yet available · ${bytes(info.budgetBytes)} budget`
          : error
            ? "Cache status is unavailable."
            : "Reading cache size…"}
      </p>
      <p className="muted">
        Clearing derived previews keeps original images, ranking sessions,
        operation journals, and recovery backups. Displayed and prefetched
        previews stay available until their views release them.
      </p>
      {info && (
        <p className="muted">
          {info.pinnedFiles} displayed or prefetched previews (
          {bytes(info.pinnedBytes)}) are pinned.
          {inventoryKnown && info.sizeBytes > info.budgetBytes && (
            <>
              {" "}
              Cache use is {bytes(info.sizeBytes - info.budgetBytes)} above the
              budget; pinned previews are retained until released.
            </>
          )}
        </p>
      )}
      {info && (
        <label className="field">
          Cache budget
          <select
            value={info.budgetBytes}
            disabled={busy}
            onChange={(event) =>
              void mutate("budget", Number(event.target.value))
            }
          >
            {[
              ...new Set([
                128 * 1048576,
                512 * 1048576,
                1024 * 1048576,
                2048 * 1048576,
                info.budgetBytes,
              ]),
            ]
              .sort((a, b) => a - b)
              .map((value) => (
                <option key={value} value={value}>
                  {bytes(value)}
                </option>
              ))}
          </select>
        </label>
      )}
      <button disabled={busy || !info} onClick={() => void mutate("clear")}>
        {busy && <LoaderCircle size={15} className="spin" />} Clear preview
        cache
      </button>{" "}
      {canRetry && (
        <button disabled={busy} onClick={() => void mutate("retry")}>
          Retry clear request
        </button>
      )}{" "}
      <button disabled={busy || reading} onClick={() => void refresh(true)}>
        {reading ? (
          <LoaderCircle size={15} className="spin" />
        ) : (
          <RefreshCw size={15} />
        )}{" "}
        Refresh cache status
      </button>
      {(clear.attempted || maintenanceActive || (info && !inventoryKnown)) && (
        <p role="status" className={clearConfirmed ? "success" : "muted"}>
          {clearConfirmed ? (
            <>
              Unused cache entries cleared.{" "}
              {inventoryKnown ? (
                <>
                  {info!.files} files remain, including {info!.pinnedFiles}{" "}
                  displayed or prefetched previews.
                </>
              ) : (
                <>Current cache inventory is not yet verified.</>
              )}{" "}
              Other previews regenerate when requested.
            </>
          ) : error ? (
            <>
              Cache maintenance status is unavailable. Refresh to confirm
              whether cleanup finished.
            </>
          ) : clear.attempted && !clear.confirmed ? (
            <>
              Clear request outcome is unconfirmed. Refresh cache status to look
              up this request; an idle cache does not confirm completion.
            </>
          ) : outcome?.state === "cancelled" ? (
            <>
              Clear request was cancelled. Retained items have not been
              confirmed removed. Retry this request to resume cleanup.
            </>
          ) : outcome?.state === "failed" ? (
            <>
              Clear request failed. Retained items have not been confirmed
              removed. Retry this request to resume cleanup.
            </>
          ) : outcome &&
            ["completed-with-retained-items", "completed"].includes(
              outcome.state,
            ) ? (
            <>
              Clear finished with retained items or an unverified retained
              count. Review the details below before retrying.
            </>
          ) : outcome?.state === "waiting-for-pins" ? (
            <>
              Clear request is waiting for displayed previews or retained
              resources to be released.
            </>
          ) : outcome?.state === "running" || info?.maintenanceRunning ? (
            <>
              Cache maintenance is running. Displayed and prefetched previews
              remain available.
            </>
          ) : outcome?.state === "pending" || info?.maintenancePending ? (
            <>
              {clear.confirmed ? "Clear request accepted. " : ""}Cache
              maintenance is queued. Displayed and prefetched previews remain
              available.
            </>
          ) : (
            <>
              Waiting for a verified cache inventory before confirming cleanup.
            </>
          )}
        </p>
      )}
      {outcome && (
        <div aria-label="Clear request details">
          {!clear.confirmed && (
            <p className="muted">
              Last reported details; the current outcome is unconfirmed.
            </p>
          )}
          <p className="muted">
            {outcome.retainedCountsKnown
              ? `${outcome.retainedFiles} retained cache files (${bytes(outcome.retainedBytes)}).`
              : "Retained cache counts are not yet verified."}
          </p>
          {!!outcome.reasons.length && (
            <ul>
              {outcome.reasons.map((reason, index) => (
                <li key={index}>{reason}</li>
              ))}
            </ul>
          )}
        </div>
      )}
      {!!info?.retirementIssues?.length && (
        <div className="notice" aria-label="Cache retirement warnings">
          <p>Native cache retirement warnings:</p>
          <ul>
            {info.retirementIssues.map((reason, index) => (
              <li key={index}>{reason}</li>
            ))}
          </ul>
        </div>
      )}
      {pollingPaused && (
        <p className="muted">
          Automatic refresh is paused. Use Refresh cache status to check again.
        </p>
      )}
      {error && (
        <p role="alert" className="error">
          {error}
        </p>
      )}
    </section>
  );
}
