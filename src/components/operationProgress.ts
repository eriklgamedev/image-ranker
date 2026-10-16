import type {
  Operation,
  OperationProgress,
  OperationRowProgress,
} from "../native";

/** Consume every sequence, but clone large row arrays at most once per paint batch. */
export function operationProgressReceiver(
  initial: Operation | null,
  publish: (operation: Operation) => void,
  read: (id: string) => Promise<Operation>,
  reportError: (message: string) => void,
  interval = 50,
) {
  let current = initial;
  let sequence = initial?.sequence ?? 0;
  let last: OperationProgress | null = null;
  let stopped = false;
  let fetching = false;
  let overflow = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const rows = new Map<number, OperationRowProgress>();
  const waiting = new Map<number, OperationProgress>();
  const flush = () => {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
    if (stopped || !current || !last) return;
    const nextRows = rows.size ? current.rows.slice() : current.rows;
    for (const [index, change] of rows)
      nextRows[index] = {
        ...nextRows[index],
        step: change.step,
        verification: change.verification,
      };
    current = {
      ...current,
      rows: nextRows,
      sequence,
      status: last.status,
      completed: last.completed,
      historyRetired: last.historyRetired,
      errors: last.errors,
    };
    rows.clear();
    last = null;
    publish(current);
  };
  const snapshot = (value: Operation) => {
    if ((value.sequence ?? 0) < sequence) return;
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
    rows.clear();
    last = null;
    current = value;
    sequence = value.sequence ?? 0;
    publish(value);
  };
  const reconcile = async (id: string) => {
    if (fetching || stopped) return;
    fetching = true;
    try {
      const value = await read(id);
      if (stopped) return;
      if (value.id !== id || !Number.isSafeInteger(value.sequence))
        throw new Error(
          "Operation progress could not be reconciled with its journal.",
        );
      snapshot(value);
      const queued = [...waiting.values()].sort(
        (a, b) => a.sequence - b.sequence,
      );
      waiting.clear();
      fetching = false;
      if (overflow) {
        overflow = false;
        void reconcile(id);
      }
      for (const event of queued) accept(event, false);
    } catch (reason) {
      fetching = false;
      if (!stopped)
        reportError(
          `Progress refresh failed: ${String(reason)}. The file journal remains authoritative.`,
        );
    }
  };
  const accept = (event: OperationProgress, refreshMissing = true) => {
    if (
      stopped ||
      (current && event.id !== current.id) ||
      event.sequence <= sequence
    )
      return;
    if (event.snapshot) {
      snapshot({ ...event.snapshot, sequence: event.sequence });
      for (const key of waiting.keys())
        if (key <= sequence) waiting.delete(key);
      return;
    }
    if (
      !current ||
      event.sequence !== sequence + 1 ||
      event.total !== current.rows.length ||
      event.rows.some(
        (row) =>
          !Number.isSafeInteger(row.index) ||
          row.index < 0 ||
          row.index >= event.total,
      )
    ) {
      waiting.set(event.sequence, event);
      if (waiting.size > 2000) {
        waiting.clear();
        overflow = true;
      }
      if (refreshMissing) void reconcile(event.id);
      return;
    }
    sequence = event.sequence;
    last = event;
    for (const row of event.rows) rows.set(row.index, row);
    if (timer === undefined) timer = setTimeout(flush, interval);
  };
  return {
    accept,
    flush,
    stop() {
      stopped = true;
      if (timer !== undefined) clearTimeout(timer);
      rows.clear();
      waiting.clear();
    },
  };
}
