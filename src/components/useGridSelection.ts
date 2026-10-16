import {
  useEffect,
  useRef,
  useState,
  type PointerEvent,
  type RefObject,
} from "react";

export interface GridSelection {
  selected: string[];
  selectable: string[];
  disabled: boolean;
  onStart?: () => void;
  onCommit: (ids: string[]) => void;
}

type Rectangle = { left: number; top: number; width: number; height: number };

/** Hit-test the full virtual layout, including rows that are not mounted. */
export function useGridSelection(
  viewport: RefObject<HTMLDivElement | null>,
  content: RefObject<HTMLDivElement | null>,
  ids: string[],
  columns: number,
  cardWidth: number,
  rowSize: number,
  selection?: GridSelection,
) {
  const [preview, setPreview] = useState<{
    rectangle: Rectangle;
    selected: Set<string>;
  } | null>(null);
  const stop = useRef<(() => void) | null>(null);
  const suppressClick = useRef(false);
  // Array identity changes on ordinary App renders; only semantic changes
  // invalidate a gesture. This also catches filtering and scan arrivals.
  const scope = JSON.stringify([
    ids,
    columns,
    cardWidth,
    rowSize,
    selection?.disabled,
    selection?.selectable,
    selection?.selected,
  ]);
  const current = useRef({ scope, selection });
  current.current = { scope, selection };

  useEffect(() => {
    stop.current?.();
    return () => stop.current?.();
  }, [scope]);

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    suppressClick.current = false;
    if (
      !selection ||
      selection.disabled ||
      event.pointerType !== "mouse" ||
      event.button !== 0 ||
      event.isPrimary === false ||
      stop.current
    )
      return;
    const target = event.target instanceof Element ? event.target : null;
    const control = target?.closest(
      "button,input,label,select,textarea,a,[contenteditable=true]",
    );
    if (control && !control.hasAttribute("data-grid-selection-surface")) return;
    const scroll = viewport.current,
      canvas = content.current;
    if (!scroll || !canvas) return;
    const bounds = scroll.getBoundingClientRect();
    // The native scrollbar is not a selection surface.
    if (
      event.clientX >= bounds.left + scroll.clientLeft + scroll.clientWidth ||
      event.clientY >= bounds.top + scroll.clientTop + scroll.clientHeight
    )
      return;

    const pointerId = event.pointerId;
    const startClient = { x: event.clientX, y: event.clientY };
    let client = startClient,
      active = false,
      frame = 0,
      lastFrame = 0;
    const allowed = new Set(selection.selectable);
    const baseline = event.ctrlKey
      ? selection.selected.filter((id) => allowed.has(id))
      : [];
    const point = () => {
      const box = canvas.getBoundingClientRect();
      return {
        x: Math.max(
          0,
          Math.min(columns * (cardWidth + 14) - 14, client.x - box.left),
        ),
        y: Math.max(
          0,
          Math.min(
            Math.ceil(ids.length / columns) * rowSize,
            client.y - box.top,
          ),
        ),
      };
    };
    const origin = point();
    const valid = () =>
      current.current.scope === scope && !current.current.selection?.disabled;
    const measure = () => {
      const end = point();
      const left = Math.min(origin.x, end.x),
        top = Math.min(origin.y, end.y);
      const right = Math.max(origin.x, end.x),
        bottom = Math.max(origin.y, end.y);
      const selected = new Set(baseline);
      ids.forEach((id, index) => {
        const x = (index % columns) * (cardWidth + 14);
        const y = Math.floor(index / columns) * rowSize;
        if (
          allowed.has(id) &&
          right > x &&
          left < x + cardWidth &&
          bottom > y &&
          top < y + rowSize - 14
        )
          selected.add(id);
      });
      return {
        rectangle: { left, top, width: right - left, height: bottom - top },
        selected,
      };
    };
    const finish = () => {
      stop.current = null;
      cancelAnimationFrame(frame);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
      window.removeEventListener("blur", cancel);
      window.removeEventListener("keydown", key);
      scroll.removeEventListener("scroll", scrolled);
      scroll.removeEventListener("lostpointercapture", cancel);
      if (scroll.hasPointerCapture?.(pointerId))
        scroll.releasePointerCapture(pointerId);
      setPreview(null);
    };
    const cancel = () => {
      if (active) suppressClick.current = true;
      finish();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        cancel();
      }
    };
    const scrolled = () => {
      if (!valid()) cancel();
      else if (active) setPreview(measure());
    };
    const autoScroll = (time: number) => {
      if (!valid()) {
        cancel();
        return;
      }
      const box = scroll.getBoundingClientRect();
      const distance =
        client.y < box.top + 28
          ? client.y - box.top - 28
          : client.y > box.bottom - 28
            ? client.y - box.bottom + 28
            : 0;
      if (distance) {
        const before = scroll.scrollTop;
        scroll.scrollTop = Math.max(
          0,
          Math.min(
            scroll.scrollHeight - scroll.clientHeight,
            before +
              Math.max(-18, Math.min(18, distance)) *
                Math.min(2, (time - lastFrame) / 16 || 1),
          ),
        );
        if (before !== scroll.scrollTop) setPreview(measure());
      }
      lastFrame = time;
      frame = requestAnimationFrame(autoScroll);
    };
    const move = (e: globalThis.PointerEvent) => {
      if (e.pointerId !== pointerId) return;
      if (!valid() || !(e.buttons & 1)) {
        cancel();
        return;
      }
      client = { x: e.clientX, y: e.clientY };
      if (
        !active &&
        Math.hypot(client.x - startClient.x, client.y - startClient.y) >= 6
      ) {
        active = true;
        suppressClick.current = true;
        selection.onStart?.();
        scroll.setPointerCapture?.(pointerId);
        frame = requestAnimationFrame(autoScroll);
      }
      if (active) {
        e.preventDefault();
        setPreview(measure());
      }
    };
    const up = (e: globalThis.PointerEvent) => {
      if (e.pointerId !== pointerId) return;
      client = { x: e.clientX, y: e.clientY };
      const next = active && valid() ? [...measure().selected] : null;
      finish();
      if (next) current.current.selection?.onCommit(next);
    };
    stop.current = cancel;
    window.addEventListener("pointermove", move, { passive: false });
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    window.addEventListener("blur", cancel);
    window.addEventListener("keydown", key);
    scroll.addEventListener("scroll", scrolled);
    scroll.addEventListener("lostpointercapture", cancel);
  };

  return {
    preview,
    onPointerDown,
    onClickCapture: (event: {
      preventDefault: () => void;
      stopPropagation: () => void;
      detail: number;
    }) => {
      if (suppressClick.current && event.detail !== 0) {
        event.preventDefault();
        event.stopPropagation();
      }
    },
    onDragStartCapture: (event: { preventDefault: () => void }) => {
      if (stop.current) event.preventDefault();
    },
  };
}
