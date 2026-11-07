// SPDX-License-Identifier: MIT
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";

import { appPopoverBoundsElements, type PopoverBounds } from "@/shared/lib/popover-bounds";
export type { PopoverBounds } from "@/shared/lib/popover-bounds";

import { ROW_POPUP_ATTR, isCmdW } from "@/shared/lib/focus-utils";

/** A row popup's size: its width, and the height of its body (the part a drag resizes). */
export interface PopoverSize {
  width: number;
  height: number;
}

const EDGE_PX = 8;
/** The gap between the row and the popup, which its arrow spans. */
const GAP_PX = 8;
/** How close the arrow comes to the popup's rounded corners. */
const ARROW_INSET_PX = 14;
/** How far one arrow key press on the grip resizes the popup. */
const KEY_STEP_PX = 16;
/** How long after its last arrow key a keyboard resize stops holding the popup's place. */
const KEY_HOLD_MS = 1500;

/** Where the popup sits: beside its row, to the right or (with no room there) to the left. The
 *  arrow on the side facing the row points at the row's middle, `arrowTop` down from the top;
 *  `side` is null when the popup covers the row and has no side facing it, or when the row's
 *  middle is above or below the popup. */
export interface PopoverPlace {
  top: number;
  left: number;
  side: "left" | "right" | null;
  arrowTop: number;
  maxWidth: number;
  maxHeight: number;
}

/** The largest side a saved size may have: past it the value is not one a drag could reach. */
const MAX_SAVED_PX = 20000;

/** A side a drag could have saved: a finite number of pixels above zero, below any screen's. */
const plausible = (n: unknown): n is number =>
  typeof n === "number" && Number.isFinite(n) && n > 0 && n <= MAX_SAVED_PX;

function readSavedSize(key: string, fallback: PopoverSize): PopoverSize {
  try {
    const saved = JSON.parse(localStorage.getItem(key) ?? "null") as Record<string, unknown> | null;
    if (plausible(saved?.width) && plausible(saved?.height)) {
      return { width: saved.width, height: saved.height };
    }
  } catch {
    // No storage: the default size.
  }
  return fallback;
}

function saveSize(key: string, size: PopoverSize): void {
  try {
    localStorage.setItem(key, JSON.stringify(size));
  } catch {
    // No storage: the size lasts until the popup closes.
  }
}

/** `size` kept between `min` and the room the window has (`maxWidth` × `maxHeight`). The room
 *  wins: in a small window the popup shrinks below its minimum rather than leave the window. */
function clampSize(
  size: PopoverSize,
  min: PopoverSize,
  maxWidth: number,
  maxHeight: number,
): PopoverSize {
  return {
    width: Math.max(0, Math.min(Math.max(min.width, size.width), maxWidth)),
    height: Math.max(0, Math.min(Math.max(min.height, size.height), maxHeight)),
  };
}

/** The same place: the popup's position and its arrow's. */
const samePlace = (a: PopoverPlace | null, b: PopoverPlace): boolean =>
  !!a &&
  a.top === b.top &&
  a.left === b.left &&
  a.side === b.side &&
  a.arrowTop === b.arrowTop &&
  a.maxWidth === b.maxWidth &&
  a.maxHeight === b.maxHeight;

/** The first row still in view in the list, to take focus when the popup's row scrolls away (or
 *  closes with its row no longer drawn). */
export function rowInView(scroller: Element): HTMLElement | null {
  const box = scroller.getBoundingClientRect();
  for (const row of Array.from(scroller.querySelectorAll(".ag-row"))) {
    const r = row.getBoundingClientRect();
    if (r.bottom <= box.top || r.top >= box.bottom) continue;
    const target = row.querySelector<HTMLElement>("button, [tabindex]:not([tabindex='-1'])");
    if (target) return target;
  }
  return null;
}

/** The list itself, made focusable (by script only) when it is not already. */
function focusableList(scroller: Element | null): HTMLElement | null {
  if (!(scroller instanceof HTMLElement) || !scroller.isConnected) return null;
  if (!scroller.hasAttribute("tabindex")) scroller.tabIndex = -1;
  return scroller;
}

/** Where focus goes when the popup closes with focus inside it, and not back to its row: a row
 *  still in view, else the list, else the row's own button while it is still there. `toRow`
 *  puts the row's button first (the popup closed with its row still listed). Null only when none
 *  of them is left, never the page body, where the keys would reach nothing. */
function focusAfterClose(
  scroller: Element | null,
  anchor: HTMLElement,
  toRow: boolean,
): HTMLElement | null {
  if (toRow && anchor.isConnected) return anchor;
  const row = scroller?.isConnected ? rowInView(scroller) : null;
  return row ?? focusableList(scroller) ?? (anchor.isConnected ? anchor : null);
}

/**
 * The frame the Agents pane's row popups share (the quick view and the inbox chat): anchored
 * beside the row (on the side with room, an arrow pointing back at it) and following it as
 * anything around it, or the popup itself, scrolls or resizes, closed by Escape
 * (focus back to the row), ⌘W with focus anywhere in it, an outside click or the row scrolling
 * away, and resized from the bottom-right grip by pointer or arrow keys, the size remembered under
 * `storageKey` on this Mac. The dialog spreads `closeProps`, which marks it as a row popup: it
 * closes on ⌘W, and no shortcut acts on the CLI or the workspaces behind it while it has focus.
 */
export function useRowPopover({
  anchor,
  onClose,
  storageKey,
  defaultSize,
  minSize,
  chromePx = 0,
  bounds,
  keepEscape,
}: {
  /** The row's button: the popup sits beside its row and returns focus to it. */
  anchor: HTMLElement;
  /** `restoreFocus` is true for Escape, false for an outside click or a scroll away. */
  onClose: (restoreFocus: boolean) => void;
  storageKey: string;
  defaultSize: PopoverSize;
  minSize: PopoverSize;
  /** The popup's height past its body (header, footer, gaps and padding) until it is measured:
   *  wrapped lines and notes make it taller. */
  chromePx?: number;
  /** The host content rectangle, excluding window chrome and the workspace bar. */
  bounds?: () => PopoverBounds;
  /** True when this Escape belongs to what has focus inside the popup, not to the popup. */
  keepEscape?: (dialog: HTMLElement) => boolean;
}) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  // The chrome as drawn: the dialog's height past its body's, or all of it when it has no body
  // (the quick view without a live terminal), so no body height is kept for it.
  const [measuredChrome, setMeasuredChrome] = useState<number | null>(null);
  const chrome = measuredChrome ?? chromePx;
  // Host layout changes refit the remembered size to the current content rectangle.
  const [, setViewport] = useState(0);
  const boundsNow = () =>
    bounds?.() ?? { top: 0, left: 0, right: window.innerWidth, bottom: window.innerHeight };
  const fit = (size: PopoverSize): PopoverSize => {
    const b = boundsNow();
    return clampSize(
      size,
      minSize,
      b.right - b.left - 2 * EDGE_PX,
      b.bottom - b.top - chrome - 2 * EDGE_PX,
    );
  };
  const [place, setPlace] = useState<PopoverPlace | null>(null);
  // The place as last set, for the resize handlers, which outlive the render that bound them.
  const placeRef = useRef<PopoverPlace | null>(null);
  // The side the popup sits on, before the arrow is hidden for a row outside its span.
  const besideRef = useRef<PopoverPlace["side"]>(null);
  // While the grip is dragged or resized by key: where the popup's corner and side stay.
  const [held, setHeld] = useState<{
    top: number;
    left: number;
    side: PopoverPlace["side"];
  } | null>(null);
  const heldRef = useRef(held);
  const [size, setSize] = useState<PopoverSize>(() => readSavedSize(storageKey, defaultSize));
  // Another popup's key (the same hook reused for another kind) reads that kind's saved size.
  const [sizeKey, setSizeKey] = useState(storageKey);
  if (sizeKey !== storageKey) {
    setSizeKey(storageKey);
    setSize(readSavedSize(storageKey, defaultSize));
  }
  const shown = fit(size);
  shown.width = Math.min(shown.width, place?.maxWidth ?? shown.width);
  shown.height = Math.min(shown.height, place?.maxHeight ?? shown.height);

  const sizeRef = useRef(size);
  const fitRef = useRef(fit);
  const keepRef = useRef(keepEscape);
  const onCloseRef = useRef(onClose);
  const chromeRef = useRef(chrome);
  const boundsRef = useRef(boundsNow);
  const storageKeyRef = useRef(storageKey);
  useLayoutEffect(() => {
    storageKeyRef.current = storageKey;
    sizeRef.current = size;
    fitRef.current = fit;
    keepRef.current = keepEscape;
    onCloseRef.current = onClose;
    chromeRef.current = chrome;
    boundsRef.current = boundsNow;
    heldRef.current = held;
  });

  useLayoutEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const measure = () => {
      const body = bodyRef.current;
      const px = dialog.offsetHeight - (body?.offsetHeight ?? 0);
      if (px > 0) setMeasuredChrome((was) => (was === px ? was : px));
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(dialog);
    return () => observer.disconnect();
  }, []);

  // Beside the row, to its right (to its left when only that side has room), moved along with any
  // scroll or resize around it and placed again when the popup's own size changes. While its grip
  // is dragged, or resized by its arrow keys, it keeps its left edge and its side, so the corner
  // follows the pointer and the popup never flips sides under focus; it is placed beside the row
  // again when the drag ends, or when the grip loses focus or its keys rest. The arrow shows only while the row's middle is level
  // with the popup. A row that scrolls out of the list closes the popup; focus inside it goes to a
  // row still in view, and so does a scroll of the list that leaves the row undrawn. The listeners
  // read the size and the held place through refs, so a drag's every move does not subscribe them
  // again: `placeNow` places it again when either changes.
  const placeNow = useRef<(() => void) | null>(null);
  useLayoutEffect(() => {
    let row: Element = anchor.closest(".ag-row") ?? anchor;
    const scroller = anchor.closest(".ag-scroll");
    // The list redraws a row under the same key (`data-row-key`): the popup follows it there.
    const rowKey = anchor.closest<HTMLElement>("[data-row-key]")?.dataset.rowKey ?? null;
    /** The row as the list draws it now, else null while it draws none (a folded card, a row
     *  outside the drawn window, one being dragged). */
    const rowNow = (): Element | null => {
      if (row.isConnected) return row;
      if (rowKey === null || !scroller?.isConnected) return null;
      const drawn = Array.from(scroller.querySelectorAll<HTMLElement>("[data-row-key]")).find(
        (el) => el.dataset.rowKey === rowKey,
      );
      if (!drawn) return null;
      row = drawn.querySelector(".ag-row") ?? drawn;
      observer?.observe(row);
      return row;
    };
    const place = () => {
      // With no row drawn there is no box to sit beside (its rect is all zeros): the popup holds
      // the place it last had, and follows the row again once the list draws it.
      const current = rowNow();
      const b = boundsRef.current();
      if (!current && placeRef.current) {
        const previous = placeRef.current;
        const next = {
          ...previous,
          maxWidth: Math.max(minSize.width, b.right - previous.left - EDGE_PX),
          maxHeight: Math.max(
            minSize.height,
            b.bottom - previous.top - chromeRef.current - EDGE_PX,
          ),
        };
        if (!samePlace(previous, next)) {
          placeRef.current = next;
          setPlace(next);
        }
        return;
      }
      const r = (current ?? row).getBoundingClientRect();
      const box = fitRef.current(sizeRef.current);
      const held = heldRef.current;
      const height = (bodyRef.current ? box.height : 0) + chromeRef.current;
      const minHeight = (bodyRef.current ? minSize.height : 0) + chromeRef.current;
      const top = Math.max(
        b.top + EDGE_PX,
        Math.min(held?.top ?? r.top, b.bottom - (held ? minHeight : height) - EDGE_PX),
      );
      const minLeft = b.left + EDGE_PX;
      const maxLeft = b.right - box.width - EDGE_PX;
      const right = r.right + GAP_PX;
      const leftOfRow = r.left - GAP_PX - box.width;
      let left: number;
      let side: PopoverPlace["side"];
      if (held) {
        ({ left, side } = held);
        // Shrink from the held edge first; move it only when the minimum no longer fits.
        left = Math.max(minLeft, Math.min(left, b.right - minSize.width - EDGE_PX));
      } else if (right <= maxLeft || leftOfRow < minLeft) {
        left = Math.max(minLeft, Math.min(right, maxLeft));
        side = left >= r.right ? "left" : null;
      } else {
        left = leftOfRow;
        side = "right";
      }
      const maxWidth = Math.max(minSize.width, b.right - left - EDGE_PX);
      const maxHeight = Math.max(minSize.height, b.bottom - top - chromeRef.current - EDGE_PX);
      const drawnHeight =
        (bodyRef.current ? Math.min(box.height, maxHeight) : 0) + chromeRef.current;
      besideRef.current = side;
      // No arrow for a row above or below the popup, or one a held popup has grown over.
      const middle = r.top + r.height / 2;
      if (middle < top || middle > top + drawnHeight) side = null;
      if (side === "right" && left + Math.min(box.width, maxWidth) > r.left) side = null;
      if (side === "left" && left < r.right) side = null;
      const arrowTop = Math.max(
        ARROW_INSET_PX,
        Math.min(middle - top, drawnHeight - ARROW_INSET_PX),
      );
      const next = { top, left, side, arrowTop, maxWidth, maxHeight };
      // Every scroll and resize places it again: a render only when the place moved.
      if (samePlace(placeRef.current, next)) return;
      placeRef.current = next;
      setPlace(next);
    };
    const closeAway = () => {
      const dialog = dialogRef.current;
      if (dialog?.contains(document.activeElement)) {
        focusAfterClose(scroller, anchor, false)?.focus({ preventScroll: true });
      }
      onCloseRef.current(false);
    };
    const onScroll = (e: Event) => {
      // Only a scroll of something the row sits in moves it (not one inside the popup). With no
      // row drawn, something its list sits in.
      const target = e.target;
      const within = rowNow() ?? scroller;
      if (target instanceof Node && target !== document && !(within && target.contains(within)))
        return;
      // Outside the list, or outside the window when the row sits in no list.
      const box = scroller?.getBoundingClientRect() ?? { top: 0, bottom: window.innerHeight };
      const current = rowNow();
      if (!current) {
        // No row drawn: a fold or a drag drew it away and the popup holds its place, but a
        // scroll of its list drew it away by scrolling it out of view (straight past the edge,
        // with no scroll seen while it was still drawn outside the list's box).
        if (scroller && target instanceof Node && target !== document && target.contains(scroller))
          closeAway();
        return;
      }
      const r = current.getBoundingClientRect();
      if (r.bottom <= box.top || r.top >= box.bottom) closeAway();
      else place();
    };
    const onResize = () => {
      setViewport((version) => version + 1);
      place();
    };
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(onResize);
    placeNow.current = place;
    place();
    window.addEventListener("resize", onResize);
    window.addEventListener("scroll", onScroll, true);
    observer?.observe(row);
    if (scroller) observer?.observe(scroller);
    if (bounds) {
      const { content, bar } = appPopoverBoundsElements();
      if (content) observer?.observe(content);
      if (bar) observer?.observe(bar);
    }
    return () => {
      window.removeEventListener("resize", onResize);
      window.removeEventListener("scroll", onScroll, true);
      observer?.disconnect();
      if (placeNow.current === place) placeNow.current = null;
    };
  }, [anchor, bounds, minSize.width, minSize.height]);
  useLayoutEffect(() => {
    placeNow.current?.();
  }, [chrome, shown.width, shown.height, held]);

  // However it closes (its agent left the list, its tab opened in the CLI), focus that was inside
  // it goes to its row, else a row or the list, never the page body. Checked once it is gone:
  // a close that gave focus back to the row, or a terminal given back to its tile with its focus,
  // has put focus somewhere already, and only focus left on the body is moved.
  const focusInside = useRef(false);
  useEffect(() => {
    // Found while the row is listed: once its agent has left, the row is no longer in the list.
    const scroller = anchor.closest(".ag-scroll");
    const onFocusIn = (e: FocusEvent) => {
      focusInside.current = e.target instanceof Node && !!dialogRef.current?.contains(e.target);
    };
    focusInside.current = !!dialogRef.current?.contains(document.activeElement);
    document.addEventListener("focusin", onFocusIn);
    return () => {
      document.removeEventListener("focusin", onFocusIn);
      const active = document.activeElement;
      if (!focusInside.current || (active && active !== document.body)) return;
      focusAfterClose(scroller, anchor, true)?.focus({ preventScroll: true });
    };
  }, [anchor]);

  // Escape, while focus is in the popup or on its row, closes it and returns focus to the row;
  // a press outside closes it without moving focus, and so does focus Tabbing out of both (where
  // its Escape could no longer reach it). A press in something the popup opened in a portal (a
  // menu, a tooltip, a select) is inside: React carries its events through the popup.
  const pressedInside = useRef(false);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // An Escape that ends an IME composition belongs to the composition.
      if (e.isComposing || e.keyCode === 229) return;
      const dialog = dialogRef.current;
      const active = document.activeElement;
      if (!dialog || !(dialog.contains(active) || active === anchor)) return;
      if (keepRef.current?.(dialog)) return;
      e.stopPropagation();
      onCloseRef.current(true);
    };
    const onPressStart = () => {
      pressedInside.current = false;
    };
    const onDown = (e: MouseEvent) => {
      if (pressedInside.current) return;
      const target = e.target as Node;
      if (anchor.closest(".ag-row")?.contains(target)) return;
      if (dialogRef.current?.contains(target)) return;
      // A portal the popup opened (a menu, a select) whose press React does not carry through
      // the popup (its own root, or a press it stopped) is inside too.
      if (target instanceof Element && target.closest("[data-radix-popper-content-wrapper]"))
        return;
      onCloseRef.current(false);
    };
    const onFocusIn = (e: FocusEvent) => {
      const target = e.target;
      if (!(target instanceof Node) || target === document.body) return;
      if (dialogRef.current?.contains(target)) return;
      if ((anchor.closest(".ag-row") ?? anchor).contains(target)) return;
      // A portal the popup opened (a menu, a select) holds its own focus.
      if (target instanceof Element && target.closest("[data-radix-popper-content-wrapper]"))
        return;
      onCloseRef.current(false);
    };
    document.addEventListener("keydown", onKey, true);
    document.addEventListener("mousedown", onPressStart, true);
    document.addEventListener("mousedown", onDown);
    document.addEventListener("focusin", onFocusIn);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("mousedown", onPressStart, true);
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("focusin", onFocusIn);
    };
  }, [anchor]);
  const onMouseDown = () => {
    pressedInside.current = true;
  };

  // A drag in progress: unmounting mid-drag ends it and keeps the size reached.
  const dragRef = useRef<(() => void) | null>(null);
  // A keyboard resize holds the popup's left edge and side as a drag does, until the grip loses
  // focus or the keys rest for KEY_HOLD_MS.
  const keyHoldRef = useRef<number | undefined>(undefined);
  // A size the arrow keys reached and not yet saved: saved once, when the key is let go.
  const keySizeRef = useRef<PopoverSize | null>(null);
  const saveKeySize = () => {
    if (keySizeRef.current) saveSize(storageKey, keySizeRef.current);
    keySizeRef.current = null;
  };
  const releaseKeyHold = () => {
    if (keyHoldRef.current === undefined) return;
    window.clearTimeout(keyHoldRef.current);
    keyHoldRef.current = undefined;
    if (!dragRef.current) setHeld(null);
  };
  useEffect(
    () => () => {
      dragRef.current?.();
      window.clearTimeout(keyHoldRef.current);
      if (keySizeRef.current) saveSize(storageKeyRef.current, keySizeRef.current);
    },
    [],
  );

  /** `next` kept between the minimum and the room from the popup's top-left corner to the
   *  window's edges: a drag or an arrow key grows it only as far as the window shows it. */
  const fitFromCorner = (next: PopoverSize): PopoverSize =>
    clampSize(
      next,
      minSize,
      boundsRef.current().right - (placeRef.current?.left ?? 0) - EDGE_PX,
      boundsRef.current().bottom - (placeRef.current?.top ?? 0) - chromeRef.current - EDGE_PX,
    );

  const startResize = (e: React.PointerEvent<HTMLElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    dragRef.current?.();
    const grip = e.currentTarget;
    grip.setPointerCapture?.(e.pointerId);
    const from = { x: e.clientX, y: e.clientY, ...shown };
    // The size the user drags to, kept whole: a small window shows less of it, and a larger
    // one shows it all again. A press that never moves leaves the saved size as it was.
    let intended: PopoverSize | null = null;
    if (placeRef.current)
      setHeld({ top: placeRef.current.top, left: placeRef.current.left, side: besideRef.current });
    const onMove = (m: PointerEvent) => {
      intended = {
        width: Math.max(minSize.width, from.width + m.clientX - from.x),
        height: Math.max(minSize.height, from.height + m.clientY - from.y),
      };
      setSize(fitFromCorner(intended));
    };
    const end = () => {
      dragRef.current = null;
      grip.removeEventListener("pointermove", onMove);
      grip.removeEventListener("pointerup", end);
      grip.removeEventListener("pointercancel", end);
      grip.removeEventListener("lostpointercapture", end);
      if (intended) saveSize(storageKey, intended);
      setHeld(null);
    };
    dragRef.current = end;
    grip.addEventListener("pointermove", onMove);
    grip.addEventListener("pointerup", end);
    grip.addEventListener("pointercancel", end);
    grip.addEventListener("lostpointercapture", end);
  };

  // The arrow keys resize from the grip too: left and right its width, up and down its height.
  const resizeByKey = (e: React.KeyboardEvent<HTMLElement>) => {
    // A chord (⌘, ⌥ or ⌃ with an arrow) is a shortcut, not a resize.
    if (e.metaKey || e.altKey || e.ctrlKey) return;
    const step = e.shiftKey ? KEY_STEP_PX * 4 : KEY_STEP_PX;
    const delta = {
      ArrowLeft: [-step, 0],
      ArrowRight: [step, 0],
      ArrowUp: [0, -step],
      ArrowDown: [0, step],
    }[e.key];
    if (!delta) return;
    e.preventDefault();
    e.stopPropagation();
    if (keyHoldRef.current === undefined && !dragRef.current && placeRef.current) {
      setHeld({ top: placeRef.current.top, left: placeRef.current.left, side: besideRef.current });
    }
    window.clearTimeout(keyHoldRef.current);
    keyHoldRef.current = window.setTimeout(releaseKeyHold, KEY_HOLD_MS);
    // From the size the last press reached: a held key repeats faster than the popup re-renders.
    const from = keySizeRef.current ?? shown;
    const next = fitFromCorner({ width: from.width + delta[0], height: from.height + delta[1] });
    setSize(next);
    keySizeRef.current = next;
  };
  const endKeyResize = (e: React.KeyboardEvent<HTMLElement>) => {
    if (e.key.startsWith("Arrow")) saveKeySize();
  };
  const gripHintId = useId();

  /** The grip's props: a focusable resize handle the pointer drags and the arrow keys move, a
   *  separator between the popup and the space it can grow into. It has nothing to press, so it is
   *  no button. A focusable separator carries a value: the popup's width, between its minimum and
   *  the window's room (the text says both sides; ↑↓ change the height). The popup draws
   *  `gripHint`, which describes it, inside it. */
  const gripProps = {
    role: "separator",
    tabIndex: 0,
    "aria-label": "Resize popup",
    "aria-describedby": gripHintId,
    "aria-orientation": "vertical" as const,
    "aria-valuenow": Math.round(shown.width),
    "aria-valuemin": Math.min(minSize.width, Math.round(shown.width)),
    "aria-valuemax": Math.max(
      Math.round(shown.width),
      boundsNow().right - boundsNow().left - 2 * EDGE_PX,
    ),
    "aria-valuetext": `${Math.round(shown.width)} by ${Math.round(shown.height)} pixels`,
    title: "Drag to resize, or use the arrow keys",
    onPointerDown: startResize,
    onKeyDown: resizeByKey,
    onKeyUp: endKeyResize,
    onBlur: () => {
      saveKeySize();
      releaseKeyHold();
    },
  };
  /** The grip's description, read with it: drawn hidden inside the grip. */
  const gripHint = { id: gripHintId, text: "Arrow keys change its size; Shift moves further." };

  /** The arrow's props: on the side facing the row, at the row's middle. */
  const arrowProps = {
    className: "ag-popover-arrow",
    "aria-hidden": true,
    "data-side": place?.side ?? undefined,
    hidden: !place?.side,
    style: { top: place?.arrowTop ?? 0 },
  };

  /** The row popup's marker and its ⌘W: it closes the popup (focus back to the row), and the
   *  app's ⌘W, which reads the same chord and sees the marker, closes nothing behind it. */
  const closeProps = {
    [ROW_POPUP_ATTR]: "",
    onKeyDown: (e: React.KeyboardEvent) => {
      if (!isCmdW(e)) return;
      e.preventDefault();
      e.stopPropagation();
      onCloseRef.current(true);
    },
  };

  return {
    dialogRef,
    bodyRef,
    place,
    shown,
    gripProps,
    gripHint,
    arrowProps,
    onMouseDown,
    closeProps,
  };
}
