import { useEffect, useRef, useState } from "react";
import { LoaderCircle, RefreshCw } from "lucide-react";
import { api, desktop, type ProtectionStatus } from "../native";

const readNativeStatus = () => api.protectionStatus();
const labels: Record<ProtectionStatus["state"], string> = {
  ready: "Protection capacity available",
  "ordinary-limit": "Ordinary admission limit reached",
  "recovery-limit": "Recovery publication limit reached",
  unavailable: "Protection capacity unavailable",
};
const pair = (bytes: number, claims: number) =>
  `${bytes.toLocaleString()} bytes · ${claims.toLocaleString()} claims`;

export function ProtectionStatusPanel({
  readStatus = readNativeStatus,
}: {
  readStatus?: () => Promise<ProtectionStatus>;
}) {
  const [snapshot, setSnapshot] = useState<ProtectionStatus | null>(null);
  const [error, setError] = useState("");
  const [reading, setReading] = useState(false);
  const [attempted, setAttempted] = useState(false);
  const alive = useRef(false),
    generation = useRef(0),
    inFlight = useRef(false);
  useEffect(() => {
    if (!desktop) return;
    alive.current = true;
    setSnapshot(null);
    setError("");
    setAttempted(false);
    setReading(inFlight.current);
    return () => {
      alive.current = false;
      generation.current++;
    };
  }, [readStatus]);

  async function check() {
    if (!alive.current || inFlight.current) return;
    inFlight.current = true;
    const admitted = generation.current;
    setReading(true);
    setAttempted(true);
    setSnapshot(null);
    setError("");
    try {
      const next = await readStatus();
      if (alive.current && admitted === generation.current) setSnapshot(next);
    } catch (cause) {
      if (alive.current && admitted === generation.current)
        setError(String(cause));
    } finally {
      inFlight.current = false;
      if (alive.current) setReading(false);
    }
  }

  if (!desktop) return null;
  const capacity =
    snapshot?.state !== "unavailable" ? snapshot?.capacity : null;
  const unavailable = !!error || (!!snapshot && !capacity);
  const state = unavailable ? "unavailable" : snapshot?.state;
  return (
    <section className="support-section" aria-label="Protection capacity">
      <h3>Protection capacity</h3>
      <p className="muted">
        Shared protection records track source identities and recovery files
        across profiles. Check their capacity when an operation reports a
        protection limit.
      </p>
      <button disabled={reading} onClick={() => void check()}>
        {reading ? (
          <LoaderCircle className="spin" size={15} />
        ) : (
          <RefreshCw size={15} />
        )}{" "}
        {reading
          ? "Checking protection capacity…"
          : attempted
            ? "Refresh protection capacity"
            : "Check protection capacity"}
      </button>
      {reading ? (
        <p role="status">Reading current protection capacity…</p>
      ) : state ? (
        <p role="status">{labels[state]}</p>
      ) : (
        <p className="muted">Capacity has not been checked.</p>
      )}
      {unavailable && (
        <p className="muted">
          No current capacity reading is available. Refresh to try again.
        </p>
      )}
      {snapshot?.message && (
        <p className={state === "ready" ? "muted" : "notice"}>
          {snapshot.message}
        </p>
      )}
      {error && (
        <p className="error" role="alert">
          Protection capacity could not be read: {error}
        </p>
      )}
      {capacity && (
        <>
          <dl
            className="support-limits"
            aria-label="Observed protection capacity"
          >
            <dt>Recorded usage</dt>
            <dd>{pair(capacity.usedBytes, capacity.usedClaims)}</dd>
            <dt>Hard limit</dt>
            <dd>{pair(capacity.hardBytes, capacity.hardClaims)}</dd>
            <dt>Ordinary admission ceiling</dt>
            <dd>
              {pair(capacity.ordinaryByteLimit, capacity.ordinaryClaimLimit)}
            </dd>
            <dt>Ordinary headroom</dt>
            <dd>
              {pair(
                capacity.ordinaryBytesRemaining,
                capacity.ordinaryClaimsRemaining,
              )}
            </dd>
            <dt>Recovery publication headroom</dt>
            <dd>
              {pair(
                capacity.publicationBytesRemaining,
                capacity.publicationClaimsRemaining,
              )}
            </dd>
            <dt>Reserved completions</dt>
            <dd>
              {pair(
                capacity.reservedCompletionBytes,
                capacity.reservedCompletionClaims,
              )}
            </dd>
            <dt>Emergency allowance</dt>
            <dd>
              {pair(
                capacity.emergencyByteAllowance,
                capacity.emergencyClaimAllowance,
              )}
            </dd>
            <dt>Maintenance allowance</dt>
            <dd>
              {pair(
                capacity.maintenanceByteAllowance,
                capacity.maintenanceClaimAllowance,
              )}
            </dd>
          </dl>
          <p className="muted">
            Observed headroom does not guarantee that the next operation fits.
            Concurrent profiles can change these values. Allowances are reserved
            limits, not additional free space.
          </p>
        </>
      )}
      <p className="muted">
        Protection storage maintenance can compact records while preserving
        historical protections. Use its separate preview and approval flow; this
        capacity check makes no storage changes.
      </p>
    </section>
  );
}
