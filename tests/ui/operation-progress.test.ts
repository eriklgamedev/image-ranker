import { afterEach, describe, expect, it, vi } from "vitest";
import { operationProgressReceiver } from "../../src/components/operationProgress";
import type { Operation, OperationProgress } from "../../src/native";
function operation(sequence = 0, count = 10000): Operation {
  return {
    id: "op",
    kind: "copy",
    status: "copying",
    sequence,
    rows: Array.from({ length: count }, (_, index) => ({
      imageId: String(index),
      from: `from/${index}`,
      to: `to/${index}`,
      temporary: `tmp/${index}`,
      rank: index + 1,
      outputNumber: index + 1,
      sizeBytes: 3,
      issue: null,
      noop: false,
      step: "planned",
    })),
    errors: [],
    completed: 0,
    manifestPath: null,
    order: 1,
    dependencies: [],
  };
}
function event(sequence: number, index = 0): OperationProgress {
  return {
    id: "op",
    sequence,
    status: "copying",
    completed: sequence,
    total: 10000,
    rows: [{ index, step: "copied", verification: "streamed-sha256" }],
    errors: [],
    historyRetired: false,
    snapshot: null,
  };
}
afterEach(() => vi.useRealTimers());
describe("compact operation progress", () => {
  it("consumes all deltas but paints once per batch without mutating the approved plan", () => {
    vi.useFakeTimers();
    const initial = operation();
    const publish = vi.fn();
    const read = vi.fn();
    const receiver = operationProgressReceiver(initial, publish, read, vi.fn());
    for (let seq = 1; seq <= 1000; seq++) receiver.accept(event(seq, seq - 1));
    expect(publish).not.toHaveBeenCalled();
    vi.advanceTimersByTime(50);
    expect(publish).toHaveBeenCalledTimes(1);
    const result = publish.mock.calls[0][0] as Operation;
    expect(result.sequence).toBe(1000);
    expect(result.rows[999].step).toBe("copied");
    expect(initial.rows[999].step).toBe("planned");
    expect(result.rows[1000]).toBe(initial.rows[1000]);
    receiver.accept(event(900));
    vi.advanceTimersByTime(100);
    expect(publish).toHaveBeenCalledTimes(1);
    expect(read).not.toHaveBeenCalled();
    receiver.stop();
  });
  it("refetches once for a sequence gap and applies later queued deltas", async () => {
    vi.useFakeTimers();
    const publish = vi.fn();
    let finish!: (op: Operation) => void;
    const read = vi.fn(
      () =>
        new Promise<Operation>((resolve) => {
          finish = resolve;
        }),
    );
    const receiver = operationProgressReceiver(
      operation(),
      publish,
      read,
      vi.fn(),
    );
    receiver.accept(event(3, 2));
    receiver.accept(event(4, 3));
    expect(read).toHaveBeenCalledTimes(1);
    const fetched = operation(3);
    fetched.rows[0].step = "copied";
    fetched.rows[2].step = "copied";
    finish(fetched);
    await Promise.resolve();
    receiver.flush();
    const last = publish.mock.calls.at(-1)![0] as Operation;
    expect(last.sequence).toBe(4);
    expect(last.rows[0].step).toBe("copied");
    expect(last.rows[3].step).toBe("copied");
    receiver.stop();
  });
  it("uses terminal snapshots for full errors and ignores stale asynchronous refresh after teardown", async () => {
    vi.useFakeTimers();
    const publish = vi.fn();
    let finish!: (op: Operation) => void;
    const receiver = operationProgressReceiver(
      null,
      publish,
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
      vi.fn(),
    );
    receiver.accept(event(4));
    const terminal = {
      ...operation(5),
      status: "complete",
      errors: Array.from({ length: 25 }, (_, i) => `Issue ${i}`),
    };
    receiver.accept({ ...event(5), snapshot: terminal });
    expect(publish.mock.calls.at(-1)![0].errors).toHaveLength(25);
    receiver.stop();
    finish(operation(4));
    await Promise.resolve();
    vi.runAllTimers();
    expect(publish).toHaveBeenCalledTimes(1);
  });
  it("surfaces refresh failures and never publishes an incomplete event as a full operation", async () => {
    const publish = vi.fn();
    const error = vi.fn();
    const receiver = operationProgressReceiver(
      null,
      publish,
      async () => {
        throw new Error("offline");
      },
      error,
    );
    receiver.accept(event(8));
    await Promise.resolve();
    await Promise.resolve();
    expect(error).toHaveBeenCalledWith(
      expect.stringContaining("Progress refresh failed"),
    );
    expect(publish).not.toHaveBeenCalled();
    receiver.stop();
  });
});
