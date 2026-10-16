import { afterEach, describe, expect, it, vi } from "vitest";
import {
  PreviewScheduler,
  PREVIEW_CONCURRENCY,
  PREVIEW_WAIT_LIMIT,
  PREVIEW_DEADLINE_MS,
  previewError,
} from "../../src/components/previewScheduler";
const flush = async () => {
  for (let i = 0; i < 12; i++) await Promise.resolve();
};
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("bounded preview scheduling", () => {
  it("expires a request whose queue wait alone consumes its total deadline", async () => {
    vi.useFakeTimers();
    const queue = new PreviewScheduler(),
      controller = new AbortController();
    const finishes: Array<() => void> = [];
    const blockers = Array.from({ length: PREVIEW_CONCURRENCY }, () =>
      queue
        .schedule(
          () => new Promise<void>((resolve) => finishes.push(resolve)),
          "current",
          controller.signal,
        )
        .catch((error) => error),
    );
    const run = vi.fn(async () => "ready"),
      settled = vi.fn();
    const waiting = queue
      .schedule(run, "visible", controller.signal)
      .then(settled, settled);
    await flush();
    try {
      await vi.advanceTimersByTimeAsync(PREVIEW_DEADLINE_MS + 1);
      expect(settled).toHaveBeenCalledWith(
        expect.objectContaining({ code: "PREVIEW_TIMEOUT", retryable: true }),
      );
      expect(run).not.toHaveBeenCalled();
    } finally {
      controller.abort();
      finishes.forEach((finish) => finish());
      await Promise.all([...blockers, waiting]);
    }
  });
  it("expires lower-priority consumers under continuous current arrivals instead of starving forever", async () => {
    vi.useFakeTimers();
    const queue = new PreviewScheduler();
    const finishes: Array<() => void> = [];
    const current = () =>
      queue.schedule(
        () => new Promise<void>((resolve) => finishes.push(resolve)),
        "current",
      );
    const currents = Array.from({ length: PREVIEW_CONCURRENCY }, current);
    await flush();
    const visible = vi.fn(async () => "visible"),
      prefetch = vi.fn(async () => "prefetch");
    const outcomes = [
      queue.schedule(visible, "visible"),
      queue.schedule(prefetch, "prefetch"),
    ].map((promise) => promise.catch((error) => error.code));
    for (let second = 1; second < 30; second++) {
      await vi.advanceTimersByTimeAsync(1000);
      currents.push(...Array.from({ length: PREVIEW_CONCURRENCY }, current));
      finishes
        .slice((second - 1) * PREVIEW_CONCURRENCY, second * PREVIEW_CONCURRENCY)
        .forEach((finish) => finish());
      await flush();
      expect(visible).not.toHaveBeenCalled();
      expect(prefetch).not.toHaveBeenCalled();
      expect(vi.getTimerCount()).toBe(1);
    }
    await vi.advanceTimersByTimeAsync(1000);
    expect(await Promise.all(outcomes)).toEqual([
      "PREVIEW_TIMEOUT",
      "PREVIEW_TIMEOUT",
    ]);
    finishes.slice(-PREVIEW_CONCURRENCY).forEach((finish) => finish());
    await Promise.all(currents);
    await flush();
    expect(vi.getTimerCount()).toBe(0);
  });
  it("retains an expired active slot through actual return and asynchronous late lease disposal", async () => {
    vi.useFakeTimers();
    const queue = new PreviewScheduler();
    let finishFirst!: (value: string) => void,
      finishRelease!: () => void,
      firstSignal!: AbortSignal;
    const discard = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          finishRelease = resolve;
        }),
    );
    const first = queue
      .schedule(
        (signal) => {
          firstSignal = signal;
          return new Promise<string>((resolve) => {
            finishFirst = resolve;
          });
        },
        "current",
        undefined,
        discard,
      )
      .catch((error) => error);
    await flush();
    await vi.advanceTimersByTimeAsync(1000);
    const finishOthers: Array<() => void> = [],
      otherSignals: AbortSignal[] = [];
    const others = Array.from({ length: PREVIEW_CONCURRENCY - 1 }, () =>
      queue.schedule((signal) => {
        otherSignals.push(signal);
        return new Promise<void>((resolve) => finishOthers.push(resolve));
      }, "visible"),
    );
    await flush();
    await vi.advanceTimersByTimeAsync(PREVIEW_DEADLINE_MS - 1000);
    expect(await first).toMatchObject({
      code: "PREVIEW_TIMEOUT",
      retryable: true,
      automatic: false,
    });
    expect(firstSignal.aborted).toBe(true);
    expect(otherSignals.every((signal) => !signal.aborted)).toBe(true);
    const nextRun = vi.fn(async () => "next"),
      next = queue.schedule(nextRun, "current");
    await flush();
    expect(nextRun).not.toHaveBeenCalled();
    finishFirst("late-owned-lease");
    await flush();
    expect(discard).toHaveBeenCalledWith("late-owned-lease");
    expect(nextRun).not.toHaveBeenCalled();
    finishRelease();
    await flush();
    expect(await next).toBe("next");
    finishOthers.forEach((finish) => finish());
    await Promise.all(others);
    await flush();
    expect(vi.getTimerCount()).toBe(0);
  });
  it.each([29_850, 29_950])(
    "does not reset admission budget after %dms waiting then retry backoff",
    async (elapsed) => {
      vi.useFakeTimers();
      const queue = new PreviewScheduler(),
        finishes: Array<() => void> = [];
      const blockers = Array.from({ length: PREVIEW_CONCURRENCY }, () =>
        queue
          .schedule(
            () => new Promise<void>((resolve) => finishes.push(resolve)),
            "current",
          )
          .catch((error) => error),
      );
      let finishAttempt!: () => void;
      const run = vi
        .fn()
        .mockRejectedValueOnce(new Error("DECODE_BUSY"))
        .mockImplementation(
          () =>
            new Promise<void>((resolve) => {
              finishAttempt = resolve;
            }),
        );
      const result = queue.schedule(run, "visible").catch((error) => error);
      await flush();
      await vi.advanceTimersByTimeAsync(elapsed);
      finishes[0]();
      await flush();
      expect(run).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(PREVIEW_DEADLINE_MS - elapsed);
      expect(await result).toMatchObject({
        code: "PREVIEW_TIMEOUT",
        retryable: true,
      });
      expect(run).toHaveBeenCalledTimes(elapsed === 29_850 ? 2 : 1);
      finishAttempt?.();
      finishes.slice(1).forEach((finish) => finish());
      await Promise.all(blockers);
      await flush();
      expect(vi.getTimerCount()).toBe(0);
    },
  );
  it("uses monotonic elapsed time even when wall time changes backward or forward", async () => {
    vi.useFakeTimers();
    const queue = new PreviewScheduler();
    let finish!: () => void;
    const settled = vi.fn();
    const result = queue
      .schedule(
        () =>
          new Promise<void>((resolve) => {
            finish = resolve;
          }),
        "current",
      )
      .then(settled, settled);
    await flush();
    vi.setSystemTime(new Date("1999-01-01"));
    await vi.advanceTimersByTimeAsync(PREVIEW_DEADLINE_MS - 1);
    expect(settled).not.toHaveBeenCalled();
    vi.setSystemTime(new Date("2099-01-01"));
    await vi.advanceTimersByTimeAsync(1);
    expect(settled).toHaveBeenCalledWith(
      expect.objectContaining({ code: "PREVIEW_TIMEOUT" }),
    );
    finish();
    await result;
    await flush();
    expect(vi.getTimerCount()).toBe(0);
  });
  it("rejects a late successful promise even if the deadline timer has not run yet", async () => {
    vi.useFakeTimers();
    const clock = vi.spyOn(performance, "now").mockReturnValue(0),
      queue = new PreviewScheduler();
    let finish!: (value: string) => void;
    const discard = vi.fn();
    const result = queue
      .schedule(
        () =>
          new Promise<string>((resolve) => {
            finish = resolve;
          }),
        "visible",
        undefined,
        discard,
      )
      .catch((error) => error);
    await flush();
    clock.mockReturnValue(PREVIEW_DEADLINE_MS);
    finish("late-lease");
    expect(await result).toMatchObject({ code: "PREVIEW_TIMEOUT" });
    await flush();
    expect(discard).toHaveBeenCalledWith("late-lease");
    expect(vi.getTimerCount()).toBe(0);
  });
  it("loads ordinary visible work promptly and never exceeds concurrency under saturation", async () => {
    const queue = new PreviewScheduler();
    let active = 0,
      high = 0;
    const finishes: Array<() => void> = [];
    const jobs = Array.from({ length: 60 }, (_, i) =>
      queue.schedule(
        () =>
          new Promise<number>((resolve) => {
            active++;
            high = Math.max(high, active);
            finishes.push(() => {
              active--;
              resolve(i);
            });
          }),
        "visible",
      ),
    );
    await flush();
    expect(finishes).toHaveLength(PREVIEW_CONCURRENCY);
    for (let i = 0; i < jobs.length; i++) {
      finishes[i]();
      await flush();
    }
    expect(await Promise.all(jobs)).toEqual(
      Array.from({ length: 60 }, (_, i) => i),
    );
    expect(high).toBe(PREVIEW_CONCURRENCY);
  });
  it("bounds waiting requests and reclaims canceled queue entries without native work", async () => {
    const queue = new PreviewScheduler(),
      controller = new AbortController();
    const work = vi.fn(async () => true);
    const jobs = Array.from({ length: PREVIEW_WAIT_LIMIT }, () =>
      queue
        .schedule(work, "visible", controller.signal)
        .catch((error) => error.code),
    );
    const overflow = queue.schedule(work, "visible");
    controller.abort();
    await expect(overflow).rejects.toMatchObject({
      code: "PREVIEW_QUEUE_FULL",
      retryable: true,
    });
    expect(await Promise.all(jobs)).toEqual(
      Array(PREVIEW_WAIT_LIMIT).fill("PREVIEW_CANCELLED"),
    );
    expect(work).not.toHaveBeenCalled();
    expect(await queue.schedule(work, "visible")).toBe(true);
  });
  it("chooses current then visible work ahead of prefetch and keeps speculative concurrency at one", async () => {
    const queue = new PreviewScheduler(),
      calls: string[] = [],
      finishes: Array<() => void> = [];
    const run = (name: string) => () =>
      new Promise<void>((resolve) => {
        calls.push(name);
        finishes.push(resolve);
      });
    const preA = queue.schedule(run("prefetch-A"), "prefetch");
    const preB = queue.schedule(run("prefetch-B"), "prefetch");
    const visible = queue.schedule(run("visible"), "visible");
    const current = queue.schedule(run("current"), "current");
    await flush();
    expect(calls).toEqual(["current", "visible", "prefetch-A"]);
    finishes[0]();
    finishes[1]();
    await flush();
    expect(calls).toHaveLength(3);
    finishes[2]();
    await flush();
    expect(calls.at(-1)).toBe("prefetch-B");
    finishes[3]();
    await Promise.all([preA, preB, visible, current]);
  });
  it("does not reuse a canceled native slot until that request actually returns, and disposes late results", async () => {
    const queue = new PreviewScheduler(),
      controller = new AbortController(),
      release = vi.fn();
    const finishes: Array<() => void> = [];
    const run = () => new Promise<void>((resolve) => finishes.push(resolve));
    const first = queue
      .schedule(run, "current", controller.signal, release)
      .catch((error) => error.code);
    const others = Array.from({ length: PREVIEW_CONCURRENCY }, () =>
      queue.schedule(run, "visible"),
    );
    await flush();
    controller.abort();
    await flush();
    expect(await first).toBe("PREVIEW_CANCELLED");
    expect(finishes).toHaveLength(PREVIEW_CONCURRENCY);
    finishes[0]();
    await flush();
    expect(finishes).toHaveLength(PREVIEW_CONCURRENCY + 1);
    expect(release).toHaveBeenCalledTimes(1);
    finishes.slice(1).forEach((finish) => finish());
    await Promise.all(others);
  });
  it("admits a current comparison even when lower-priority work filled the waiting queue", async () => {
    const queue = new PreviewScheduler(),
      controller = new AbortController();
    const work = vi.fn(async () => "ready");
    const speculative = Array.from({ length: PREVIEW_WAIT_LIMIT }, () =>
      queue
        .schedule(work, "prefetch", controller.signal)
        .catch((error) => error.code),
    );
    const current = queue.schedule(work, "current");
    expect(await current).toBe("ready");
    controller.abort();
    expect(await Promise.all(speculative)).toContain("PREVIEW_QUEUE_FULL");
  });
  it.each([
    "IMAGE_JOB_CAPACITY",
    "IMAGE_WORK_BUSY",
    "DECODE_BUSY",
    "DECODE_CAPACITY",
  ])("retries %s finitely and permits a later direct retry", async (code) => {
    vi.useFakeTimers();
    const queue = new PreviewScheduler(),
      run = vi.fn().mockRejectedValue(new Error(`${code}: busy`));
    const failed = queue.schedule(run, "visible").catch((error) => error);
    await vi.runAllTimersAsync();
    expect(run).toHaveBeenCalledTimes(4);
    expect(await failed).toMatchObject({ code, retryable: true });
    run.mockResolvedValue("ready");
    const retried = queue.schedule(run, "visible");
    await vi.runAllTimersAsync();
    expect(await retried).toBe("ready");
  });
  it("cancels retry backoff without issuing another request", async () => {
    vi.useFakeTimers();
    const queue = new PreviewScheduler(),
      controller = new AbortController();
    const run = vi.fn().mockRejectedValue(new Error("DECODE_BUSY"));
    const pending = queue
      .schedule(run, "visible", controller.signal)
      .catch((error) => error.code);
    await vi.advanceTimersByTimeAsync(0);
    controller.abort();
    await vi.runAllTimersAsync();
    expect(await pending).toBe("PREVIEW_CANCELLED");
    expect(run).toHaveBeenCalledTimes(1);
  });
  it.each([
    "CONTENT_CHANGED",
    "MISSING_SOURCE",
    "STALE_CONTEXT",
    "DECODE_UNAVAILABLE",
    "DECODE_CANCELLED",
    "IMAGE_JOB_CANCELLED",
    "PREVIEW_CAPACITY",
  ])(
    "does not automatically retry permanent or intervention-required %s",
    async (code) => {
      const queue = new PreviewScheduler(),
        run = vi.fn().mockRejectedValue(new Error(`${code}: explanation`));
      await expect(queue.schedule(run, "visible")).rejects.toMatchObject({
        code,
        automatic: false,
      });
      expect(run).toHaveBeenCalledTimes(1);
    },
  );
  it("does not classify explanatory text containing a transient code as retryable", () => {
    expect(
      previewError(new Error("Unexpected response mentions DECODE_BUSY")),
    ).toMatchObject({ automatic: false, retryable: false });
  });
});
