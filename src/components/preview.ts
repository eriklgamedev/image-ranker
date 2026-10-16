import { convertFileSrc } from "@tauri-apps/api/core";
import { api, type ImageItem } from "../native";
import {
  cancelledPreview,
  PreviewError,
  PreviewScheduler,
  type PreviewPriority,
} from "./previewScheduler";

const scheduler = new PreviewScheduler();
export interface PreviewOptions {
  signal?: AbortSignal;
  priority?: PreviewPriority;
}

export function boundPreview(
  item: ImageItem,
  size: number,
  options: PreviewOptions = {},
) {
  // Capture identity at the consumer boundary; queued requests cannot silently
  // follow mutations of a shared image object or a retired native context.
  const captured = { ...item };
  const context = api.rendererContext();
  // Attach immediately even when this consumer waits behind other previews.
  void context.catch(() => {});
  return scheduler.schedule(
    async (signal) => {
      const admittedContext = await context;
      if (signal.aborted) throw cancelledPreview();
      if ((await api.rendererContext()) !== admittedContext)
        throw new PreviewError(
          "STALE_CONTEXT",
          "Reload the selected profile before loading images.",
        );
      return acquirePreview(captured, size, admittedContext, signal);
    },
    options.priority || (size >= 2048 ? "current" : "visible"),
    options.signal,
    (preview) => preview.release().catch(() => {}),
  );
}

async function acquirePreview(
  item: ImageItem,
  size: number,
  context: string,
  signal?: AbortSignal,
) {
  const requestId = crypto.randomUUID();
  const cancel = () => {
    void api.cancelImageWork(requestId).catch(() => {});
  };
  if (signal?.aborted) throw cancelledPreview();
  signal?.addEventListener("abort", cancel, { once: true });
  let response;
  try {
    response = await api.requestPreview(
      item.id,
      size,
      item.contentRevision,
      requestId,
    );
  } finally {
    signal?.removeEventListener("abort", cancel);
  }
  let releasePromise: Promise<void> | undefined;
  const release = () => {
    if (!releasePromise)
      releasePromise =
        typeof response.leaseId === "string" && response.leaseId.trim()
          ? Promise.resolve().then(() => api.releasePreview(response.leaseId))
          : Promise.resolve();
    return releasePromise;
  };
  try {
    if (signal?.aborted) throw cancelledPreview();
    if ((await api.rendererContext()) !== context)
      throw new PreviewError(
        "STALE_CONTEXT",
        "Ignored a preview from the previous profile.",
      );
  } catch (error) {
    await release().catch(() => {});
    throw error;
  }
  if (
    response.status !== "ready" ||
    response.requestId !== requestId ||
    response.contentRevision !== item.contentRevision ||
    typeof response.leaseId !== "string" ||
    !response.leaseId.trim() ||
    typeof response.assetToken !== "string" ||
    !response.assetToken ||
    typeof response.path !== "string" ||
    !response.path ||
    !Number.isSafeInteger(response.width) ||
    !(response.width > 0) ||
    !Number.isSafeInteger(response.height) ||
    !(response.height > 0) ||
    !Number.isFinite(response.scale) ||
    !(response.scale > 0) ||
    typeof response.pipelineVersion !== "string" ||
    !response.pipelineVersion
  ) {
    await release().catch(() => {});
    throw new PreviewError(
      "INVALID_PREVIEW",
      "The image preview no longer matches this comparison. Reconnect its source folder and retry.",
    );
  }
  try {
    return {
      response,
      url: `${convertFileSrc(response.path)}?asset=${encodeURIComponent(response.assetToken)}&request=${encodeURIComponent(requestId)}`,
      release,
    };
  } catch (error) {
    await release().catch(() => {});
    throw error;
  }
}

export type BoundPreview = Awaited<ReturnType<typeof boundPreview>>;

/** Keep speculative opponents pinned only for the current arena effect. */
export function prefetchPreviews(items: ImageItem[], size = 2048) {
  let active = true;
  const controller = new AbortController();
  const held = new Set<BoundPreview>();
  for (const item of items.slice(0, 2)) {
    if (item.availability && item.availability !== "ready") continue;
    void boundPreview(item, size, {
      priority: "prefetch",
      signal: controller.signal,
    })
      .then((preview) => {
        if (active) held.add(preview);
        else void preview.release().catch(() => {});
      })
      .catch(() => {});
  }
  return () => {
    active = false;
    controller.abort();
    for (const preview of held) void preview.release().catch(() => {});
    held.clear();
  };
}
