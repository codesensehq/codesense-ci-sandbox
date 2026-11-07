// SPDX-License-Identifier: MIT
import { useEffect, useLayoutEffect, useRef, type HTMLAttributes } from "react";

import {
  terminalManager,
  type DisplayHandle,
  type DisplayOpts,
  type TerminalManager,
  type TerminalDisplayReference,
} from "./terminal-manager";
import { TERMINAL_FONT_FAMILY } from "./terminal-constants";

/** Surfaces at or below this many pixels are hidden or mid-animation slivers:
 *  fitting to them yields a garbage column count. */
const MIN_VALID_SURFACE_PX = 2;

/** The slice of `FitAddon` a fit target needs. */
interface ProposesDimensions {
  proposeDimensions(): { cols?: number; rows?: number } | undefined;
}

/**
 * The cols/rows `container` can hold, or `null` when it can't be measured:
 * hidden, a sliver, or no cell size yet (the web font still loading). Only
 * positive integers count, so a fit converges (`term.cols === target.cols`).
 */
export function computeFitTarget(
  container: Pick<HTMLElement, "clientWidth" | "clientHeight">,
  fit: ProposesDimensions,
): { cols: number; rows: number } | null {
  if (
    container.clientWidth <= MIN_VALID_SURFACE_PX ||
    container.clientHeight <= MIN_VALID_SURFACE_PX
  ) {
    return null;
  }
  let proposed: { cols?: number; rows?: number } | undefined;
  try {
    proposed = fit.proposeDimensions();
  } catch {
    return null;
  }
  const cols = proposed?.cols;
  const rows = proposed?.rows;
  if (!Number.isInteger(cols) || !Number.isInteger(rows)) return null;
  if ((cols as number) <= 0 || (rows as number) <= 0) return null;
  return { cols: cols as number, rows: rows as number };
}

/** How long a burst of slot size changes must be quiet before it resizes. */
export const RESIZE_SETTLE_MS = 50;
/** How long a new display waits for the terminal face before measuring with
 *  the fallback face. */
export const FONT_WAIT_MS = 1000;

export interface TerminalSlotProps extends Omit<HTMLAttributes<HTMLDivElement>, "children"> {
  sessionId: TerminalDisplayReference;
  /** How the session's display is built (history length is read only once). */
  display: DisplayOpts;
  /** The slot is on screen: the active tab of a shown workspace. */
  active: boolean;
  /** Whether a reveal (attach while active, or becoming active) may take
   *  keyboard focus. Read at that moment. */
  focusOnReveal?: boolean | (() => boolean);
  /** Count the first mount while active as a reveal, so it may take focus too. A popup that
   *  opens over an already-active terminal needs this; a terminal on screen does not. */
  focusOnMount?: boolean;
  /** The slot only borrows the display (the Agents pane's quick view): unmounting gives it back
   *  to the slot it came from instead of parking it. */
  borrow?: boolean;
  /** The display is restored and at the slot's size (or the attach settled
   *  as ended or failed). Called on every mount, soon for an existing display. */
  onEnsured?: (handle: DisplayHandle) => void;
  /** The terminal face is loaded (or gave up waiting) and the slot measured:
   *  the display is at its real size from here on. */
  onMeasured?: () => void;
  /** Tests pass their own manager. */
  manager?: TerminalManager;
}

/** The terminal face at `fontSizePx` is loaded, or `FONT_WAIT_MS` passed.
 *  Resolves `true` only when the face itself loaded. */
function waitForFont(fontSizePx: number): Promise<boolean> {
  const load = document.fonts?.load?.(`${fontSizePx}px '${TERMINAL_FONT_FAMILY}'`);
  if (!load) return Promise.resolve(false);
  return Promise.race([
    load.then(
      () => true,
      () => false,
    ),
    new Promise<boolean>((resolve) => setTimeout(() => resolve(false), FONT_WAIT_MS)),
  ]);
}

/**
 * Where a session's terminal is shown. The slot owns no terminal: mounting it
 * moves the session's one display in, unmounting gives it back to the shown slot
 * it took it from (a tile under a grid cell or an expanded view), else parks it.
 * Remounting is free.
 * It is also the size owner: its settled pixel size is the only thing that
 * resizes the session. Moves, reveals, focus and renderer swaps never do.
 */
export function TerminalSlot({
  sessionId,
  display,
  active,
  focusOnReveal = true,
  focusOnMount = false,
  borrow = false,
  onEnsured,
  onMeasured,
  manager = terminalManager,
  style,
  ...divProps
}: TerminalSlotProps) {
  const ref = useRef<HTMLDivElement>(null);
  const displayRef = useRef(display);
  const activeRef = useRef(active);
  const focusRef = useRef(focusOnReveal);
  const borrowRef = useRef(borrow);
  const onEnsuredRef = useRef(onEnsured);
  const onMeasuredRef = useRef(onMeasured);
  /** No measurement until the terminal face is ready (or waited out). */
  const fontReadyRef = useRef(false);
  const life = display.life ?? 0;

  const mayFocus = () => {
    if (!activeRef.current) return false;
    const policy = focusRef.current;
    return typeof policy === "function" ? policy() : policy;
  };

  /** Ask for the size the slot holds, if it is the display's one slot, is
   *  measurable, and holds a different size. */
  const measure = () => {
    const fit = manager.fitAddon(sessionId);
    const el = ref.current;
    if (!fit || !el || !fontReadyRef.current || !manager.holds(sessionId, el)) return;
    const target = computeFitTarget(el, fit);
    const current = manager.size(sessionId);
    if (!target || (current && target.cols === current.cols && target.rows === current.rows)) {
      return;
    }
    manager.requestResize(sessionId, target.cols, target.rows);
  };
  const measureRef = useRef(measure);

  useLayoutEffect(() => {
    displayRef.current = display;
    activeRef.current = active;
    focusRef.current = focusOnReveal;
    borrowRef.current = borrow;
    onEnsuredRef.current = onEnsured;
    onMeasuredRef.current = onMeasured;
    measureRef.current = measure;
  });

  // Attach before paint, so the first frame already shows the display.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    let live = true;
    const pending = manager.ensure(sessionId, displayRef.current);
    manager.attach(sessionId, el, {
      focus: mayFocus,
      visible: activeRef.current,
      borrow: borrowRef.current,
    });
    // A restored display is built at the size it was recorded at: ask for
    // the slot's size, and report it once there, so the first frame shown is
    // not a reflow.
    void pending
      .then(async (handle) => {
        if (!live || !handle) return null;
        measureRef.current();
        await manager.sized(sessionId);
        return handle;
      })
      .then((handle) => {
        if (live && handle) onEnsuredRef.current?.(handle);
      });
    // A reconnect rebuilds without an `ensure` of ours: its outcome is ours too.
    // A display the manager puts in this slot itself (a revived eviction, a
    // reconnect's swap) is built at the session's recorded size, and the
    // slot's size changes it missed while empty were never measured.
    const unlisten = manager.listen(sessionId, {
      rebuilt: (handle) => onEnsuredRef.current?.(handle),
      placed: () => measureRef.current(),
    });
    return () => {
      live = false;
      unlisten();
      manager.detach(sessionId, el);
    };
  }, [manager, sessionId, life]);

  // Shown or hidden without unmounting (an ancestor's display:none). `shownBefore` is null
  // until the first run: only a slot that was hidden and is shown again counts as re-shown.
  const shownBeforeRef = useRef<boolean | null>(null);
  const focusOnMountRef = useRef(focusOnMount);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    manager.setVisible(sessionId, el, active);
    const reshown = active && shownBeforeRef.current === false;
    const revealed =
      reshown || (active && shownBeforeRef.current === null && focusOnMountRef.current);
    shownBeforeRef.current = active;
    // A slot shown again takes its display back if another slot took it while this one was
    // hidden (the quick view, or a newer slot): the latest reveal wins. A slot that stays shown
    // never reclaims it, so two shown slots of one session don't take it from each other. A
    // borrower (the quick view) takes it back as a borrower, so the slot it came from still
    // gets it back on close.
    if (reshown && !manager.holds(sessionId, el)) {
      manager.attach(sessionId, el, {
        focus: mayFocus,
        visible: true,
        borrow: borrowRef.current,
      });
    }
    if (revealed) manager.focus(sessionId, mayFocus);
    // Redraw once the slot is laid out: a canvas that was hidden can show blank until it is drawn.
    if (!reshown) return;
    const frame = requestAnimationFrame(() => manager.repaint(sessionId, el));
    return () => cancelAnimationFrame(frame);
  }, [manager, sessionId, active]);

  // Size owner: a change in the slot's pixels, once quiet for
  // `RESIZE_SETTLE_MS` (or at the end of a CSS transition), resizes the
  // session. Until then the display keeps its cols/rows, anchored so the
  // bottom row stays in view.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const flush = () => {
      if (timer !== null) clearTimeout(timer);
      timer = null;
      measureRef.current();
    };
    const observer = new ResizeObserver(() => {
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(flush, RESIZE_SETTLE_MS);
    });
    observer.observe(el);
    el.addEventListener("transitionend", flush);
    return () => {
      if (timer !== null) clearTimeout(timer);
      observer.disconnect();
      el.removeEventListener("transitionend", flush);
    };
  }, []);

  // Font size and theme apply to the live display. The first measurement
  // waits for the face; a new font size is a new geometry and measures again.
  const { fontSizePx, themeConfig } = display;
  useEffect(() => {
    manager.setAppearance(sessionId, { fontSizePx, themeConfig });
  }, [manager, sessionId, fontSizePx, themeConfig]);
  useEffect(() => {
    let cancelled = false;
    const settle = (loaded: boolean) => {
      if (cancelled) return;
      if (loaded) {
        manager.clearTextureAtlas(sessionId);
        manager.markAtlasBuilt(sessionId, fontSizePx);
      }
      const first = !fontReadyRef.current;
      fontReadyRef.current = true;
      measureRef.current();
      if (first) onMeasuredRef.current?.();
    };
    // A display whose atlas was built for this face and size (a team coming back, a tile
    // re-mounted) has nothing to wait for or redo: measure at once, with no atlas rebuild.
    if (manager.atlasBuilt(sessionId, fontSizePx)) settle(false);
    else void waitForFont(fontSizePx).then(settle);
    return () => {
      cancelled = true;
    };
  }, [manager, sessionId, fontSizePx]);

  // A fit holds whole cells only, so up to a cell's height (and width) of the
  // slot lies past the last row (and column). The slot paints the display's
  // background there; otherwise what shows through is whatever is behind it,
  // a dark strip under a remote pane's tinted theme. A caller's style wins.
  const background = themeConfig?.theme.background;
  return (
    <div
      ref={ref}
      data-surface="terminal"
      {...divProps}
      style={background ? { backgroundColor: background, ...style } : style}
    />
  );
}
