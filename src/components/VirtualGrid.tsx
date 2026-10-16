import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useGridSelection, type GridSelection } from "./useGridSelection";
export interface GridFocusRequest {
  id: string;
  selector: string;
  origin: HTMLElement;
}
export function VirtualGrid({
  ids,
  size,
  extraHeight = 64,
  focusRequest,
  selection,
  render,
}: {
  ids: string[];
  size: number;
  extraHeight?: number;
  focusRequest?: GridFocusRequest | null;
  selection?: GridSelection;
  render: (id: string, index: number, selected?: boolean) => ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null),
    contentRef = useRef<HTMLDivElement>(null),
    [width, setWidth] = useState(800);
  useEffect(() => {
    if (!ref.current) return;
    const observer = new ResizeObserver((entries) =>
      setWidth(entries[0].contentRect.width),
    );
    observer.observe(ref.current);
    return () => observer.disconnect();
  }, []);
  const columns = Math.max(1, Math.floor((width + 14) / (size + 14))),
    cardWidth = (width - 14 * (columns - 1)) / columns;
  const rowSize = Math.max(100, cardWidth) + extraHeight + 14;
  const dragSelection = useGridSelection(
    ref,
    contentRef,
    ids,
    columns,
    cardWidth,
    rowSize,
    selection,
  );
  const virtual = useVirtualizer({
    count: Math.ceil(ids.length / columns),
    getScrollElement: () => ref.current,
    estimateSize: () => rowSize,
    overscan: 3,
  });
  useEffect(() => virtual.measure(), [rowSize, virtual]);
  const settledFocus = useRef<GridFocusRequest | null>(null);
  const focusIndex = focusRequest ? ids.indexOf(focusRequest.id) : -1;
  useLayoutEffect(() => {
    if (!focusRequest || settledFocus.current === focusRequest) return;
    let frame: number | undefined;
    let attempts = 0;
    let cancelled = false;
    const stop = () => {
      cancelled = true;
      settledFocus.current = focusRequest;
      if (frame !== undefined) cancelAnimationFrame(frame);
    };
    const restore = () => {
      if (cancelled) return;
      if (
        focusIndex < 0 ||
        (document.activeElement !== document.body &&
          document.activeElement !== focusRequest.origin)
      ) {
        stop();
        return;
      }
      const cell = Array.from(
        ref.current?.querySelectorAll<HTMLElement>("[data-grid-id]") || [],
      ).find((item) => item.dataset.gridId === focusRequest.id);
      const target = cell?.querySelector<HTMLElement>(focusRequest.selector);
      if (target) {
        stop();
        target.focus({ preventScroll: true });
        target.scrollIntoView?.({ block: "nearest", inline: "nearest" });
      } else if (attempts++ < 10) {
        // Only explicit keyboard moves request this scroll. Bound the mounting
        // wait and abandon it if the user points elsewhere or changes focus.
        if (attempts === 1)
          virtual.scrollToIndex(Math.floor(focusIndex / columns), {
            align: "auto",
          });
        frame = requestAnimationFrame(restore);
      } else stop();
    };
    window.addEventListener("pointerdown", stop, true);
    restore();
    return () => {
      cancelled = true;
      if (frame !== undefined) cancelAnimationFrame(frame);
      window.removeEventListener("pointerdown", stop, true);
    };
  }, [focusRequest, focusIndex, columns, virtual]);
  return (
    <div
      className={`grid-scroll${selection ? " drag-selectable" : ""}${dragSelection.preview ? " selecting" : ""}`}
      ref={ref}
      onPointerDown={dragSelection.onPointerDown}
      onClickCapture={dragSelection.onClickCapture}
      onDoubleClickCapture={dragSelection.onClickCapture}
      onDragStartCapture={dragSelection.onDragStartCapture}
    >
      <div
        ref={contentRef}
        style={{ height: virtual.getTotalSize(), position: "relative" }}
      >
        {virtual.getVirtualItems().map((row) => (
          <div
            className="virtual-row"
            key={row.key}
            style={{
              transform: `translateY(${row.start}px)`,
              gridTemplateColumns: `repeat(${columns},minmax(0,1fr))`,
            }}
          >
            {ids
              .slice(row.index * columns, (row.index + 1) * columns)
              .map((id, col) => (
                <div key={id} data-grid-id={id}>
                  {render(
                    id,
                    row.index * columns + col,
                    dragSelection.preview?.selected.has(id),
                  )}
                </div>
              ))}
          </div>
        ))}
        {dragSelection.preview && (
          <div
            className="selection-rectangle"
            aria-hidden="true"
            style={dragSelection.preview.rectangle}
          />
        )}
      </div>
    </div>
  );
}
