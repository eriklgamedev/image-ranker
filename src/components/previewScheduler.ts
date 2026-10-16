export type PreviewPriority = "current" | "visible" | "prefetch";
export const PREVIEW_CONCURRENCY = 4;
export const PREVIEW_WAIT_LIMIT = 128;
// One monotonic budget includes admission, waiting, backoff, and native work.
// Priority affects service order, never the deadline of an admitted consumer.
export const PREVIEW_DEADLINE_MS = 30_000;
const priorityOrder = { current: 0, visible: 1, prefetch: 2 };
const retryDelays = [100, 300, 900];
const transientCodes = new Set([
  "IMAGE_JOB_CAPACITY",
  "IMAGE_WORK_BUSY",
  "DECODE_CAPACITY",
  "DECODE_BUSY",
  "DECODE_QUEUE_TIMEOUT",
]);
const manualRetryCodes = new Set([
  "PREVIEW_CAPACITY",
  "IMAGE_JOB_CANCELLED",
  "DECODE_CANCELLED",
]);
export class PreviewError extends Error {
  constructor(
    public readonly code: string,
    message: string,
    public readonly retryable = false,
    public readonly automatic = false,
  ) {
    super(message);
    this.name = "PreviewError";
  }
}
export function previewError(reason: unknown): PreviewError {
  if (reason instanceof PreviewError) return reason;
  const message = reason instanceof Error ? reason.message : String(reason);
  const code =
    /^(?:Error:\s*)?([A-Z][A-Z0-9_]+)(?::|$)/.exec(message)?.[1] ||
    "PREVIEW_FAILED";
  return new PreviewError(
    code,
    message,
    transientCodes.has(code) || manualRetryCodes.has(code),
    transientCodes.has(code),
  );
}
export const cancelledPreview = () =>
  new PreviewError(
    "PREVIEW_CANCELLED",
    "The image request is no longer needed.",
  );
const timedOutPreview = () =>
  new PreviewError(
    "PREVIEW_TIMEOUT",
    "The preview exceeded 30 seconds, including waiting. Retry when current image work finishes.",
    true,
  );

type Task = {
  run: (signal: AbortSignal) => Promise<unknown>;
  resolve: (value: unknown) => void;
  reject: (error: PreviewError) => void;
  signal?: AbortSignal;
  priority: PreviewPriority;
  attempts: number;
  readyAt: number;
  deadline: number;
  controller: AbortController;
  settled: boolean;
  abort: () => void;
  discard?: (value: unknown) => void | Promise<void>;
};

/** Bounds native requests, including canceled requests whose native worker has not returned. */
export class PreviewScheduler {
  private waiting: Task[] = [];
  private active = new Set<Task>();
  private timer: ReturnType<typeof setTimeout> | undefined;
  private scheduled = false;
  schedule<T>(
    run: (signal: AbortSignal) => Promise<T>,
    priority: PreviewPriority,
    signal?: AbortSignal,
    discard?: (value: T) => void | Promise<void>,
  ): Promise<T> {
    const deadline = performance.now() + PREVIEW_DEADLINE_MS;
    if (signal?.aborted) return Promise.reject(cancelledPreview());
    // Retrying tasks return to this same reservation; count running tasks too
    // so backoff cannot grow the waiting queue beyond its admission bound.
    if (this.waiting.length + this.active.size >= PREVIEW_WAIT_LIMIT) {
      let displaced = -1;
      for (let index = this.waiting.length - 1; index >= 0; index--) {
        if (
          priorityOrder[this.waiting[index].priority] > priorityOrder[priority]
        ) {
          displaced = index;
          break;
        }
      }
      const error = new PreviewError(
        "PREVIEW_QUEUE_FULL",
        "Too many images are waiting. Retry after current previews load.",
        true,
      );
      if (displaced < 0) return Promise.reject(error);
      this.finish(this.waiting.splice(displaced, 1)[0], error);
    }
    return new Promise<T>((resolve, reject) => {
      const task: Task = {
        run,
        priority,
        signal,
        attempts: 0,
        readyAt: 0,
        deadline,
        controller: new AbortController(),
        settled: false,
        discard: discard ? (value) => discard(value as T) : undefined,
        resolve: (value) => resolve(value as T),
        reject,
        abort: () => {
          this.waiting = this.waiting.filter((entry) => entry !== task);
          this.finish(task, cancelledPreview());
          task.controller.abort();
          this.requestPump();
        },
      };
      signal?.addEventListener("abort", task.abort, { once: true });
      this.waiting.push(task);
      this.requestPump();
    });
  }
  private finish(task: Task, error?: PreviewError, result?: unknown) {
    if (task.settled) return;
    task.settled = true;
    task.signal?.removeEventListener("abort", task.abort);
    if (error) task.reject(error);
    else task.resolve(result);
  }
  private requestPump() {
    if (this.scheduled) return;
    this.scheduled = true;
    queueMicrotask(() => {
      this.scheduled = false;
      this.pump();
    });
  }
  private expire(task: Task, now = performance.now()): boolean {
    if (!task.settled && now >= task.deadline) {
      // Reject only this consumer; keep an active slot until its work and any
      // late lease cleanup actually return. Cancellation is not exit proof.
      this.finish(task, timedOutPreview());
      task.controller.abort();
    }
    return task.settled;
  }
  private pump() {
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
    const now = performance.now();
    this.waiting = this.waiting.filter((task) => !this.expire(task, now));
    for (const task of this.active) this.expire(task, now);
    this.waiting.sort(
      (a, b) => priorityOrder[a.priority] - priorityOrder[b.priority],
    );
    while (this.active.size < PREVIEW_CONCURRENCY) {
      const prefetching = [...this.active].some(
        (task) => task.priority === "prefetch",
      );
      const index = this.waiting.findIndex(
        (task) =>
          task.readyAt <= now && (task.priority !== "prefetch" || !prefetching),
      );
      if (index < 0) break;
      const [task] = this.waiting.splice(index, 1);
      this.active.add(task);
      task.attempts++;
      void this.execute(task);
    }
    // One timer services every deadline/backoff, including deadlines while all
    // slots are occupied. Settled active work no longer needs a timer.
    const next = Math.min(
      ...this.waiting.flatMap((task) =>
        task.readyAt > now ? [task.readyAt, task.deadline] : [task.deadline],
      ),
      ...[...this.active]
        .filter((task) => !task.settled)
        .map((task) => task.deadline),
    );
    if (Number.isFinite(next))
      this.timer = setTimeout(
        () => this.pump(),
        Math.max(1, Math.ceil(next - performance.now())),
      );
  }
  private async execute(task: Task) {
    try {
      if (this.expire(task)) return;
      const result = await task.run(task.controller.signal);
      // Promise continuations can run before an overdue timer after the event
      // loop was busy; never accept late pixels just because that timer lagged.
      this.expire(task);
      if (task.settled) await task.discard?.(result);
      this.finish(task, undefined, result);
    } catch (reason) {
      this.expire(task);
      const error = previewError(reason);
      if (
        !task.settled &&
        !task.signal?.aborted &&
        error.automatic &&
        task.attempts <= retryDelays.length
      ) {
        task.readyAt = performance.now() + retryDelays[task.attempts - 1];
        this.waiting.push(task);
      } else this.finish(task, error);
    } finally {
      this.active.delete(task);
      this.requestPump();
    }
  }
}
