import { useCallback, useEffect, useRef, useState } from "react";
import { LoaderCircle } from "lucide-react";
import {
  RecoveryWorkControls,
  type RecoveryWorkActions,
} from "./RecoveryWorkControls";

/** Observes startup; only an explicit recovery action may restart native work. */
export function StartupLoading({
  watch,
  onRefresh,
  onHelp,
  work,
}: {
  watch: boolean;
  onRefresh: () => Promise<void>;
  onHelp: () => void;
  work?: RecoveryWorkActions;
}) {
  const alive = useRef(false),
    inFlight = useRef(false),
    polls = useRef(0);
  const [revision, setRevision] = useState(0),
    [reading, setReading] = useState(false),
    [paused, setPaused] = useState(false),
    [error, setError] = useState("");
  const refresh = useCallback(
    async (manual = false) => {
      if (!alive.current || inFlight.current) return;
      if (manual) {
        polls.current = 0;
        setPaused(false);
      }
      inFlight.current = true;
      setReading(true);
      setError("");
      try {
        await onRefresh();
      } catch (cause) {
        if (alive.current) {
          setError(String(cause));
          setPaused(true);
        }
      } finally {
        inFlight.current = false;
        if (alive.current) {
          setReading(false);
          setRevision((n) => n + 1);
        }
      }
    },
    [onRefresh],
  );
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    if (!watch || reading || paused) return;
    if (polls.current >= 120) {
      setPaused(true);
      return;
    }
    const timer = setTimeout(() => {
      polls.current++;
      void refresh();
    }, 1000);
    return () => clearTimeout(timer);
  }, [watch, reading, paused, revision, refresh]);
  return (
    <main className="recovery-page">
      <section className="startup-recovery" aria-busy="true">
        <h1>
          <LoaderCircle size={22} className="spin" /> Opening your workspace
        </h1>
        <p>
          Local records are still being checked. Ranking and file changes become
          available after native loading has finished.
        </p>
        <div className="button-row">
          <button disabled={reading} onClick={() => void refresh(true)}>
            Refresh workspace status
          </button>
          <button onClick={onHelp}>Help and local diagnostics</button>
        </div>
        {work && <RecoveryWorkControls actions={work} watch={watch} />}
        {paused && (
          <p>
            Automatic workspace checks are paused. Refresh to check again; this
            does not restart loading.
          </p>
        )}
        {error && (
          <p role="alert" className="error">
            {error}
          </p>
        )}
      </section>
    </main>
  );
}
