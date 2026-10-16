import { useRef, useState } from "react";
import type { ImageItem } from "../native";
import { Modal } from "./Modal";
import { ImagePreview } from "./ImagePreview";
import { basename } from "./OperationDialog";

export function RecompareDialog({
  images,
  eligible,
  onDecide,
  onClose,
}: {
  images: [ImageItem, ImageItem];
  eligible: boolean;
  onDecide: (choice: "left" | "right" | "both") => void;
  onClose: () => void;
}) {
  const identity = images
    .map((image) => `${image.id}:${image.contentRevision}`)
    .join("|");
  const [loaded, setLoaded] = useState({
    identity: "",
    left: false,
    right: false,
  });
  const submitted = useRef(false);
  const ready =
    eligible && loaded.identity === identity && loaded.left && loaded.right;
  return (
    <Modal
      title="Recompare adjacent images"
      onClose={onClose}
      wide
      footer={<button onClick={onClose}>Cancel</button>}
    >
      <div className="recompare-images">
        {images.map((item, index) => {
          const side = index === 0 ? "left" : "right";
          return (
            <div key={`${identity}:${side}`}>
              <ImagePreview
                item={item}
                size={2048}
                onReady={(value) =>
                  setLoaded((current) =>
                    current.identity === identity
                      ? { ...current, [side]: value }
                      : {
                          identity,
                          left: side === "left" && value,
                          right: side === "right" && value,
                        },
                  )
                }
              />
              <b>{basename(item.path)}</b>
            </div>
          );
        })}
      </div>
      <p className="muted">
        This changes the review order of these two images. Both marks them as an
        unresolved band; earlier comparison evidence and final picks stay in the
        saved review.
      </p>
      <p role="status" className={eligible ? "muted" : "error"}>
        {!eligible
          ? "Reconnect unchanged source images before making another comparison."
          : !ready
            ? "Waiting for both verified images to load…"
            : "Both images are ready."}
      </p>
      <div className="vote-buttons">
        {(["left", "both", "right"] as const).map((choice) => (
          <button
            key={choice}
            disabled={!ready || submitted.current}
            onClick={() => {
              if (!ready || submitted.current) return;
              submitted.current = true;
              onDecide(choice);
            }}
          >
            {choice === "left"
              ? "Prefer left"
              : choice === "right"
                ? "Prefer right"
                : "Both"}
          </button>
        ))}
      </div>
    </Modal>
  );
}
