import { useEffect, useRef, useState } from "react";
import { AlertTriangle, LoaderCircle } from "lucide-react";
import { desktop, previewUrl, type ImageItem } from "../native";
import { basename } from "./OperationDialog";
import { boundPreview, type BoundPreview } from "./preview";
import { PreviewError, previewError } from "./previewScheduler";
const available = (image: ImageItem) =>
  !image.availability || image.availability === "ready";
export function ImagePreview({
  item,
  onReady,
  className = "",
  size = 512,
}: {
  item: ImageItem;
  onReady?: (ready: boolean) => void;
  className?: string;
  size?: number;
}) {
  const [failed, setFailed] = useState(false),
    [failure, setFailure] = useState<PreviewError | null>(null),
    [display, setDisplay] = useState<{ key: string; url: string } | null>(null),
    [loading, setLoading] = useState(desktop),
    [retry, setRetry] = useState(0),
    activeKey = useRef(""),
    leaseRef = useRef<{ key: string; preview: BoundPreview } | null>(null),
    imageRef = useRef<HTMLImageElement>(null),
    readyCallback = useRef(onReady);
  readyCallback.current = onReady;
  const previewKey = `${item.id}:${item.contentRevision}:${item.previewPath}:${item.availability || "ready"}:${size}:${retry}`;
  activeKey.current = previewKey;
  const release = (key: string) => {
    if (leaseRef.current?.key !== key) return;
    void leaseRef.current.preview.release().catch(() => {});
    leaseRef.current = null;
  };
  useEffect(() => {
    let active = true;
    const controller = new AbortController();
    let lease: BoundPreview | undefined;
    activeKey.current = previewKey;
    setFailed(false);
    setFailure(null);
    readyCallback.current?.(false);
    setDisplay(null);
    if (desktop) {
      setLoading(true);
      boundPreview(item, size, { signal: controller.signal })
        .then((preview) => {
          if (!active || activeKey.current !== previewKey) {
            void preview.release().catch(() => {});
            return;
          }
          lease = preview;
          leaseRef.current = { key: previewKey, preview };
          setDisplay({ key: previewKey, url: preview.url });
        })
        .catch((reason) => {
          if (active && activeKey.current === previewKey) {
            setFailure(previewError(reason));
            setFailed(true);
          }
        })
        .finally(() => {
          if (active) setLoading(false);
        });
    } else {
      setDisplay({ key: previewKey, url: previewUrl(item) });
      setLoading(false);
    }
    return () => {
      active = false;
      controller.abort();
      if (activeKey.current === previewKey) activeKey.current = "";
      if (lease) void lease.release().catch(() => {});
      if (leaseRef.current?.key === previewKey) leaseRef.current = null;
    };
  }, [previewKey]);
  const decoded = (image: HTMLImageElement) => {
    const key = previewKey;
    const done = () => {
      if (
        activeKey.current === key &&
        !loading &&
        !failed &&
        (!desktop || leaseRef.current?.key === key)
      )
        readyCallback.current?.(available(item));
    };
    if (typeof image.decode === "function")
      image
        .decode()
        .then(done)
        .catch(() => {
          if (activeKey.current === key) {
            readyCallback.current?.(false);
            release(key);
            setFailure(
              new PreviewError(
                "PREVIEW_DISPLAY_FAILED",
                "The preview pixels could not be decoded. Retry loading this image.",
                true,
              ),
            );
            setFailed(true);
          }
        });
    else done();
  };
  const url = display?.key === previewKey ? display.url : "";
  useEffect(() => {
    if (
      !loading &&
      !failed &&
      imageRef.current?.complete &&
      imageRef.current.naturalWidth > 0
    )
      decoded(imageRef.current);
  }, [loading, failed, url]);
  const fail = () => {
    if (activeKey.current !== previewKey) return;
    readyCallback.current?.(false);
    release(previewKey);
    setDisplay(null);
    if (desktop && retry === 0 && available(item)) setRetry(1);
    else {
      setFailure(
        new PreviewError(
          "PREVIEW_DISPLAY_FAILED",
          "The preview could not be displayed. Retry loading this image.",
          true,
        ),
      );
      setFailed(true);
    }
  };
  return failed ? (
    <div className="image-error" role="alert">
      <AlertTriangle size={24} />
      <span>Preview unavailable</span>
      <small>{basename(item.path)}</small>
      <small>{failure?.message || "Refresh the source folder to retry."}</small>
      {failure?.retryable && available(item) && (
        <button
          type="button"
          onClick={(event) => {
            event.stopPropagation();
            setRetry((value) => value + 1);
          }}
        >
          Retry preview
        </button>
      )}
    </div>
  ) : !url || loading ? (
    <div className="image-loading" role="status">
      <LoaderCircle className="spin" size={24} />
      <span>Loading image…</span>
    </div>
  ) : (
    <img
      key={url}
      ref={imageRef}
      className={className}
      src={url}
      alt={basename(item.path)}
      draggable={false}
      onLoad={(event) => {
        if (!loading) decoded(event.currentTarget);
      }}
      onError={fail}
    />
  );
}
