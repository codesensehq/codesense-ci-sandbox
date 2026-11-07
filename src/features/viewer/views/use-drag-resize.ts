// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

export interface DragResizeOptions {
  /** Pure clamp/compute, typically a `computeClampedRatio` partial. */
  computeRatio: (args: {
    startRatio: number;
    startPos: number;
    currentPos: number;
    containerSize: number;
  }) => number;
  /** Read container dimension along the drag axis at pointer-down.
   *  Returning 0 aborts the drag cleanly with no `onDragStart` and no commit. */
  getContainerSize: () => number;
  /** Read the committed ratio at pointer-down. */
  getStartRatio: () => number;
  /** Pick the coord along the drag axis (clientY for vertical, clientX for horizontal). */
  getPos: (e: PointerEvent | React.PointerEvent) => number;
  /** Fires exactly once per drag, after a successful pointer-down + capture +
   *  snapshot. Symmetric with `onCleanup`. Use for "drag is starting" side
   *  effects (e.g. adding a global cursor class). NEVER fires when
   *  `onPointerDown` early-returns (re-entry, non-primary button, zero container). */
  onDragStart?: () => void;
  /** Apply the visual ratio to the DOM. Called rAF-coalesced, never during commit. */
  onVisualUpdate: (ratio: number) => void;
  /** Fires exactly once per drag IF the ratio changed, AFTER state-mutation
   *  in `onCommit` settles but BEFORE `onCleanup`. Symmetric with `onDragStart`.
   *  Always fires on EVERY release (pointerup, pointercancel, window.blur,
   *  unmount mid-drag, zero-movement no-op). Use for visual cleanup
   *  (e.g. removing a CSS variable, removing a global class). */
  onCleanup?: () => void;
  /** Commit the final ratio. Fires once on release IF
   *  `finalRatio !== getStartRatio()` (zero-movement and clamped-no-op
   *  releases skip it). NEVER fires on unmount mid-drag. `release.viaPointerUp`
   *  is false for an abort (pointercancel / window.blur), so a consumer can keep
   *  a deliberate side effect (e.g. snap-to-collapse) to a genuine release. */
  onCommit: (ratio: number, release: { viaPointerUp: boolean }) => void;
  /** SW-FX-8: fires once on a TAP — a pointer-down → pointer-up whose
   *  movement never crossed `tapThresholdPx`. Lets a divider double as a
   *  resize handle (drag) AND a toggle (click). A tap fires ONLY `onTap`:
   *  no `onDragStart`, `onVisualUpdate`, `onCommit`, or `onCleanup` (the
   *  drag never started). Fires only on a genuine `pointerup`, never on
   *  `pointercancel` / `window.blur` / unmount (those are aborts). */
  onTap?: () => void;
  /** Movement (px along the drag axis) below which a release is a tap, not
   *  a drag. `onDragStart` + visual updates are deferred until this is
   *  crossed, so a tap produces no dragging-class flash. Default 4. */
  tapThresholdPx?: number;
  /** SLW-FX-9: a CSS `cursor` value (e.g. `"row-resize"`, `"col-resize"`)
   *  applied to `document.body` for the DURATION of a drag — the deterministic,
   *  capture-independent carrier of the resize cursor window-wide. Set on the
   *  same threshold-cross that starts the drag (so a tap never flashes it) and
   *  cleared in `releaseInternals` on EVERY teardown path (pointerup,
   *  pointercancel, `window.blur`, unmount), INDEPENDENT of whether
   *  `releasePointerCapture` succeeds. This is why a stranded WebKit
   *  pointer-capture can no longer leave the cursor stuck: the visible cursor
   *  lives on `<body>` (guaranteed-cleared), not as an always-on element class.
   *  Consumers pairing this with a hover-only element cursor get both a
   *  hover affordance at rest AND a window-wide cursor during the drag. */
  dragCursor?: string;
}

/** Default tap/drag movement threshold in px (see `tapThresholdPx`). */
const DEFAULT_TAP_THRESHOLD_PX = 4;

interface DragState {
  pointerId: number;
  startPos: number;
  startRatio: number;
  containerSize: number;
  lastPos: number;
  captureEl: HTMLElement;
  pendingFrame: number | null;
  /** Latches true once movement crosses `tapThresholdPx`. While false the
   *  gesture is still a potential tap: `onDragStart` and visual updates are
   *  withheld. Drives the tap-vs-drag decision on release. */
  started: boolean;
  /** Opts snapshotted at pointer-down so mid-drag re-renders (e.g. a team
   *  switch swapping `teamKey` in the consumer's closures) can't shift
   *  where the commit lands. The whole drag is a single transaction. */
  snapshot: DragResizeOptions;
}

export interface DragResizeBindings {
  onPointerDown: (e: React.PointerEvent<HTMLElement>) => void;
  onPointerMove: (e: React.PointerEvent<HTMLElement>) => void;
  onPointerUp: (e: React.PointerEvent<HTMLElement>) => void;
  onPointerCancel: (e: React.PointerEvent<HTMLElement>) => void;
  /** SW-FX-74: arm/disarm the at-rest resize cursor on GENUINE pointer presence
   *  (see `cursor`). Spread onto the divider alongside the other handlers. */
  onPointerEnter: (e: React.PointerEvent<HTMLElement>) => void;
  onPointerLeave: (e: React.PointerEvent<HTMLElement>) => void;
  /** SW-FX-74: the at-rest resize cursor — the `dragCursor` value while the
   *  pointer genuinely rests on the element (armed via `onPointerEnter`), else
   *  `undefined`. Replaces the CSS `hover:cursor-*` that a stranded WebKit
   *  pointer-capture can re-assert: a stranded release fires no boundary events,
   *  so the cursor stays disarmed until a fresh, genuine `pointerEnter` re-arms
   *  it. Disarmed at drag-start so the `<body>` cursor is the sole carrier
   *  mid-drag. NOT a DOM prop — destructure it out and apply via `style.cursor`
   *  (e.g. `const { cursor, ...bindings } = useDragResize(...)`). */
  cursor: string | undefined;
}

/**
 * Pointer-Events drag-resize hook with rAF-coalesced visual updates and
 * single-commit-on-release semantics.
 *
 * Lifecycle (every successful drag fires these callbacks in this order):
 *  1. `onDragStart`            — once, when movement first crosses
 *                                `tapThresholdPx` (NOT at bare pointer-down)
 *  2. `onVisualUpdate(ratio)`  — per rAF during the drag (latest pos wins)
 *  3. `onCommit(finalRatio)`   — once on release, IF ratio changed
 *  4. `onCleanup`              — once on EVERY release path that STARTED a drag
 *
 * Tap path (SW-FX-8): a pointer-down → pointer-up whose movement never
 * crosses `tapThresholdPx` is a TAP, not a drag. It fires ONLY `onTap`
 * (on the genuine pointerup) — none of the four drag callbacks run, since
 * the drag never started. This lets one divider be both a resize handle
 * (drag) and a collapse toggle (click). `onDragStart`/visual updates are
 * deferred to the threshold-cross precisely so a tap leaves no flash.
 *
 * Why `onCommit` runs BEFORE `onCleanup`: consumers typically write a CSS
 * variable in `onVisualUpdate` and clear it in `onCleanup`. Calling
 * `onCommit` first lets React state catch up to the dragged ratio before
 * the visual var disappears, so the `var(--x, ${committed})` fallback
 * already reflects the new state when the override is removed. Reversed
 * order risks a 1-frame snap-back.
 *
 * Release paths for a STARTED drag (`pointerup`, `pointercancel`,
 * `window.blur`, unmount mid-drag) ALL fire `onCleanup`; only unmount
 * mid-drag skips `onCommit`. A gesture that never crossed the threshold
 * fires neither `onDragStart` nor `onCleanup` — it is a tap (`onTap`, on
 * pointerup only) or, on an abort path, nothing. Pointer-down bailouts
 * (re-entry, non-primary button, zero container) fire nothing either —
 * symmetric: cleanup is gated on a drag having started.
 *
 * The whole `opts` object is snapshotted into `dragRef` at pointer-down,
 * so mid-drag re-renders cannot shift where the commit lands.
 *
 * Callers bind the returned `{onPointerDown, onPointerMove, onPointerUp,
 * onPointerCancel}` to the divider element via React props.
 */
export function useDragResize(opts: DragResizeOptions): DragResizeBindings {
  // Latest opts mirrored via layout effect (assigning during render
  // trips react-hooks/refs and is unsafe under concurrent rendering;
  // `useLayoutEffect` flushes synchronously after commit and BEFORE
  // browser paint, so a pointerdown that arrives between commit and
  // the next paint still sees fresh opts — eliminates a theoretical
  // race where a `useEffect` (asynchronous) would leave `optsRef`
  // briefly stale). Only read at pointer-down — once the drag starts,
  // the hook uses the per-drag `snapshot` instead so mid-drag
  // re-renders cannot move the goalposts (e.g. team switch swapping
  // `teamKey` in the consumer's closures).
  const optsRef = useRef(opts);
  useLayoutEffect(() => {
    optsRef.current = opts;
  });

  // Single source of truth for an in-flight drag. `null` between drags.
  const dragRef = useRef<DragState | null>(null);

  // SW-FX-74: whether the pointer GENUINELY rests on the divider — the arm
  // signal for the at-rest resize cursor (`cursor`, below). Set true only on a
  // real `pointerEnter`, false on `pointerLeave` and at drag-start. A stranded
  // WebKit capture fires no boundary events, so it can never flip this back on —
  // that is the whole fix. Toggles only on enter/leave/drag-start (never in the
  // rAF drag hot path), so the drag stays render-free.
  const [armed, setArmed] = useState(false);

  // Bound `window.blur` listener — held in a ref so the same identity
  // can be removed deterministically across release paths.
  const blurHandlerRef = useRef<(() => void) | null>(null);

  // Document-level pointerup/pointercancel listeners, registered per-gesture
  // so a release that lands OFF the divider still tears the gesture down. With
  // capture deferred to the drag threshold, a release before that point (or a
  // fast pre-threshold flick off the thin handle) would otherwise strand the
  // `dragRef` and wedge the re-entry guard. Mirrors the `blur` safety listener.
  const docUpHandlerRef = useRef<((e: PointerEvent) => void) | null>(null);
  const docCancelHandlerRef = useRef<((e: PointerEvent) => void) | null>(null);
  // Document-level pointermove, registered per-gesture. The divider's hit zone
  // is only a few px wide; a grab near its edge can leave the element BEFORE
  // movement crosses the tap threshold (capture is deferred until then), so the
  // element's own onPointerMove would stop firing and the drag would never
  // start. Tracking moves at the document level closes that gap.
  const docMoveHandlerRef = useRef<((e: PointerEvent) => void) | null>(null);

  // rAF callback: read the latest pos and push one visual update.
  const flushFrame = useCallback(() => {
    const drag = dragRef.current;
    if (!drag) return;
    drag.pendingFrame = null;
    const ratio = drag.snapshot.computeRatio({
      startRatio: drag.startRatio,
      startPos: drag.startPos,
      currentPos: drag.lastPos,
      containerSize: drag.containerSize,
    });
    drag.snapshot.onVisualUpdate(ratio);
  }, []);

  // Advance the in-flight drag to a new axis position. Shared by the element's
  // onPointerMove and the document-level safety-net pointermove so a grab that
  // leaves the thin divider before the threshold still starts + tracks. Calling
  // it twice for the same event (once via each path while over the element, or
  // after capture) is harmless: lastPos is idempotent, the `started` flip + the
  // rAF schedule are both guarded.
  const advanceTo = useCallback(
    (pos: number) => {
      const drag = dragRef.current;
      if (!drag) return;
      drag.lastPos = pos;

      // Belt-and-suspenders selection guard: apply the global no-select state
      // (`html.dragging * { user-select: none !important }`) on the FIRST move —
      // even sub-threshold — so a slow drift over a neighbouring pane can't
      // anchor a native text selection in the window before `onDragStart` lands.
      // Applied on movement (not pointerdown) so a pure tap/click never flashes
      // it. Removed in `releaseInternals` on every release path. Idempotent with
      // the consumer's own `onDragStart` add.
      document.documentElement.classList.add("dragging");

      // Tap-vs-drag gate: until movement crosses the threshold the gesture is
      // still a potential tap — withhold onDragStart + visual updates.
      if (!drag.started) {
        const threshold = drag.snapshot.tapThresholdPx ?? DEFAULT_TAP_THRESHOLD_PX;
        if (Math.abs(drag.lastPos - drag.startPos) < threshold) return;
        drag.started = true;
        // Capture the pointer only now that a real drag has begun (never on a
        // bare click) so the cursor can leave the thin divider mid-drag.
        try {
          drag.captureEl.setPointerCapture(drag.pointerId);
        } catch {
          // Best-effort leave-window safety; the drag still works without it.
        }
        drag.snapshot.onDragStart?.();
        // SW-FX-74: disarm the at-rest element cursor at the SAME threshold-cross.
        // This is load-bearing, not cosmetic: at drag-start the pointer is over
        // the divider so `armed` is already true; if a WebKit-stranded release
        // then withholds the boundary events, that stale `armed` would re-strand
        // the element cursor — the exact bug. Resetting here means only a fresh,
        // genuine `pointerEnter` after release can re-arm it. `setArmed(false)`
        // when already false is a no-op re-render (React bailout).
        setArmed(false);
        // SLW-FX-9: apply the global drag cursor at the SAME threshold-cross
        // (never on a bare click). Cleared unconditionally in releaseInternals,
        // so — unlike an always-on element cursor gated behind pointer capture —
        // it cannot be stranded by a WebKit releasePointerCapture failure.
        if (drag.snapshot.dragCursor) {
          document.body.style.cursor = drag.snapshot.dragCursor;
        }
      }

      // Coalesce: one pending frame at a time. Latest position wins.
      if (drag.pendingFrame === null) {
        drag.pendingFrame = requestAnimationFrame(flushFrame);
      }
    },
    [flushFrame],
  );

  // Internal listener teardown — does NOT call `onCleanup`. Used as a
  // building block by `release` and the unmount effect. Returns the
  // drag-state snapshot for the caller to use (e.g. to fire `onCleanup`
  // after the dragRef has already been nulled).
  const releaseInternals = useCallback((): DragState | null => {
    const drag = dragRef.current;
    if (!drag) return null;

    if (drag.pendingFrame !== null) {
      cancelAnimationFrame(drag.pendingFrame);
      drag.pendingFrame = null;
    }

    // Best-effort capture release — happy-dom + some webviews report
    // hasPointerCapture() incorrectly, so call unconditionally and
    // swallow the would-be DOMException.
    try {
      drag.captureEl.releasePointerCapture(drag.pointerId);
    } catch {
      // ignore
    }

    if (blurHandlerRef.current) {
      window.removeEventListener("blur", blurHandlerRef.current);
      blurHandlerRef.current = null;
    }

    if (docUpHandlerRef.current) {
      document.removeEventListener("pointerup", docUpHandlerRef.current);
      docUpHandlerRef.current = null;
    }
    if (docCancelHandlerRef.current) {
      document.removeEventListener("pointercancel", docCancelHandlerRef.current);
      docCancelHandlerRef.current = null;
    }
    if (docMoveHandlerRef.current) {
      document.removeEventListener("pointermove", docMoveHandlerRef.current);
      docMoveHandlerRef.current = null;
    }

    // Clear the pointerdown-time selection guard (see onPointerDown). The
    // consumer's `onCleanup` also removes `.dragging` on started drags — this
    // is the only remover on the tap / pre-threshold-abort paths.
    document.documentElement.classList.remove("dragging");

    // SLW-FX-9: clear the global drag cursor on the SAME conditions the set used
    // (a STARTED drag that opted into `dragCursor` — see advanceTo), so set and
    // clear are exactly symmetric on WHETHER they touch the cursor: a bare tap /
    // pre-threshold abort never set one and so never clears one (a pre-existing
    // body cursor survives the click). Runs on EVERY started-drag teardown path
    // (pointerup, pointercancel, window.blur, unmount) and is independent of
    // releasePointerCapture succeeding — the guaranteed reset that makes the
    // stuck-cursor bug impossible. It blanks rather than restores a prior value,
    // which is safe because nothing else holds a `document.body` cursor during a
    // divider drag (grep-verified: the hook is the only body-cursor writer).
    if (drag.started && drag.snapshot.dragCursor) {
      document.body.style.cursor = "";
    }

    dragRef.current = null;
    return drag;
  }, []);

  // Full release path: compute final ratio, commit (if changed), then
  // clean up. `onCommit` runs BEFORE `onCleanup` so React state updates
  // before the visual var override is removed (no snap-back). Both fire
  // from the same `snapshot.*` so mid-drag opts churn is irrelevant.
  //
  // `onCleanup` runs in a `finally` so a throw inside `onCommit` (e.g.
  // a Zustand invariant failure, a downstream subscriber throwing
  // synchronously) cannot strand the CSS var + global `.dragging`
  // class. Without this the entire app's cursor + text selection
  // would freeze until reload.
  const release = useCallback(
    (viaPointerUp: boolean) => {
      const drag = dragRef.current;
      if (!drag) return;

      // Tap path: the drag never started (movement stayed under threshold).
      // Tear down internals, then — only on a genuine pointerup — report the
      // tap. No onCommit/onCleanup: the onDragStart/onCleanup pair never
      // opened, so firing cleanup would be asymmetric and there's no visual
      // state to revert. pointercancel / blur / late-up are NOT taps.
      if (!drag.started) {
        releaseInternals();
        if (viaPointerUp) drag.snapshot.onTap?.();
        return;
      }

      const finalRatio = drag.snapshot.computeRatio({
        startRatio: drag.startRatio,
        startPos: drag.startPos,
        currentPos: drag.lastPos,
        containerSize: drag.containerSize,
      });
      const shouldCommit = finalRatio !== drag.startRatio;

      // Tear down listeners + null dragRef BEFORE invoking consumer
      // callbacks. This ensures any stray pointer events arriving from
      // pointer capture during `onCommit`/`onCleanup` are dropped at the
      // top-of-handler `if (!drag) return` guards (no stale move/up
      // feedback into a dying drag). NOTE: this ordering does NOT block
      // a re-entrant `pointerdown` synthesized during `onCommit` from
      // opening a brand-new drag — onCleanup would then strip the new
      // drag's CSS var. In practice `onCommit` is a Zustand setter
      // whose subscribers re-render on the next microtask, so this
      // path is unreachable today; flagged here so a future consumer
      // that fires synchronous events from `onCommit` knows to defer.
      releaseInternals();

      try {
        if (shouldCommit) {
          drag.snapshot.onCommit(finalRatio, { viaPointerUp });
        }
      } finally {
        drag.snapshot.onCleanup?.();
      }
    },
    [releaseInternals],
  );

  const onPointerDown = useCallback(
    (e: React.PointerEvent<HTMLElement>) => {
      // Primary button only. Pointer events report `button === 0` for
      // touch and the primary mouse button; non-primary buttons (right
      // click, middle click) must not initiate a UI drag.
      if (e.button !== 0) return;

      // Refuse re-entry — a second pointerdown without a corresponding
      // release would otherwise overwrite the in-flight drag's snapshot.
      if (dragRef.current) return;

      const containerSize = optsRef.current.getContainerSize();
      if (containerSize <= 0) return;

      const captureEl = e.currentTarget;
      const pointerId = e.pointerId;
      const startPos = optsRef.current.getPos(e);
      const startRatio = optsRef.current.getStartRatio();

      // NB: `setPointerCapture` is intentionally NOT called here — it is
      // deferred to `onPointerMove` once a real drag begins (threshold
      // crossed). Capturing on a bare click/tap can make WebKit (the Tauri
      // webview) drop the subsequent `pointerup`/`click`, which would break
      // tap-to-collapse. A tap never needs capture; only a drag does.

      // Snapshot the WHOLE opts object — see DragState comment for why.
      const snapshot = optsRef.current;

      dragRef.current = {
        pointerId,
        startPos,
        startRatio,
        containerSize,
        lastPos: startPos,
        captureEl,
        pendingFrame: null,
        started: false,
        snapshot,
      };

      const blurHandler = () => release(false);
      blurHandlerRef.current = blurHandler;
      window.addEventListener("blur", blurHandler);

      // Off-divider release safety net (see ref decls). The element's own
      // pointerup/cancel handlers fire first for an on-divider release and
      // null `dragRef`, making these no-ops; for an off-divider release they
      // are the only thing that tears the gesture down.
      const docUpHandler = (ev: PointerEvent) => {
        const d = dragRef.current;
        if (!d || ev.pointerId !== d.pointerId) return;
        d.lastPos = d.snapshot.getPos(ev);
        release(true);
      };
      const docCancelHandler = (ev: PointerEvent) => {
        const d = dragRef.current;
        if (!d || ev.pointerId !== d.pointerId) return;
        release(false);
      };
      // Pre-threshold move safety net (see ref decl): tracks moves even after
      // the pointer leaves the thin divider before capture engages, so an
      // edge grab still starts the drag instead of falling through to a native
      // text selection.
      const docMoveHandler = (ev: PointerEvent) => {
        const d = dragRef.current;
        if (!d || ev.pointerId !== d.pointerId) return;
        advanceTo(d.snapshot.getPos(ev));
      };
      docUpHandlerRef.current = docUpHandler;
      docCancelHandlerRef.current = docCancelHandler;
      docMoveHandlerRef.current = docMoveHandler;
      document.addEventListener("pointerup", docUpHandler);
      document.addEventListener("pointercancel", docCancelHandler);
      document.addEventListener("pointermove", docMoveHandler);

      // NB: `onDragStart` is intentionally NOT fired here — it is deferred
      // to the first move that crosses `tapThresholdPx` (in `advanceTo`), so a
      // tap (click without drag) produces no dragging-class flash.
    },
    [release, advanceTo],
  );

  const onPointerMove = useCallback(
    (e: React.PointerEvent<HTMLElement>) => {
      const drag = dragRef.current;
      if (!drag) return;
      if (e.pointerId !== drag.pointerId) return;
      advanceTo(drag.snapshot.getPos(e));
    },
    [advanceTo],
  );

  const onPointerUp = useCallback(
    (e: React.PointerEvent<HTMLElement>) => {
      const drag = dragRef.current;
      if (!drag) return;
      if (e.pointerId !== drag.pointerId) return;
      drag.lastPos = drag.snapshot.getPos(e);
      release(true);
    },
    [release],
  );

  const onPointerCancel = useCallback(
    (e: React.PointerEvent<HTMLElement>) => {
      const drag = dragRef.current;
      if (!drag) return;
      if (e.pointerId !== drag.pointerId) return;
      release(false);
    },
    [release],
  );

  // SW-FX-74: arm the at-rest resize cursor on a GENUINE pointer-enter — the one
  // signal a stranded WebKit capture cannot synthesize. Guarded on `!dragRef` so
  // an enter DURING a drag can't re-arm: under capture a pointer that leaves and
  // re-enters the divider fires `pointerenter`, which would otherwise defeat the
  // drag-start disarm and, on a stranded release, strand the element cursor
  // again. `dragRef` is null between drags, so a genuine post-release enter still
  // re-arms. No `dragCursor` gate here — the returned `cursor` is the single
  // source of truth (undefined when the consumer supplied none), so arming
  // without one is a harmless no-op that shows nothing.
  const onPointerEnter = useCallback(() => {
    if (!dragRef.current) setArmed(true);
  }, []);

  // Disarm on leave. Under active pointer capture a `pointerleave` may be
  // withheld until release — that's fine: the fix hinges on arming ONLY on a
  // fresh genuine enter, and drag-start already forced disarm, so a stranded
  // release lands in the disarmed state by construction (never relies on this).
  const onPointerLeave = useCallback(() => {
    setArmed(false);
  }, []);

  // Unmount mid-drag: tear down listeners + pending frame, fire
  // `onCleanup` so visual side effects (CSS var, global class) revert,
  // but DO NOT fire `onCommit` — an in-flight drag whose host
  // component unmounts is treated as an abort (the user's stand-in for
  // "cancel"). The persistence store IS global and could accept the
  // write, but committing a partial drag on a component lifecycle
  // boundary is a worse UX than abandoning it: in practice the user
  // is mid-gesture on a divider that's about to disappear (terminal
  // toggled off, team switched, parent rerendered with a new `key`),
  // not on a clean release. Pointer capture makes this a rare path.
  useEffect(() => {
    return () => {
      const drag = releaseInternals();
      // Only fire onCleanup if a drag actually started — otherwise
      // onDragStart never ran and there's no visual state to revert
      // (preserves the onDragStart/onCleanup symmetry; an unstarted
      // gesture interrupted by unmount is not a tap either).
      if (drag?.started) drag.snapshot.onCleanup?.();
    };
  }, [releaseInternals]);

  // SW-FX-74: the armed at-rest cursor is the SAME `dragCursor` value that
  // carries the window-wide cursor during a drag (single source — no second
  // string to drift). `undefined` when disarmed, so the consumer renders no
  // element cursor at rest and the default arrow shows. Reading live `opts`
  // (not the per-drag snapshot) is intentional and safe: `armed` is forced
  // false for the entire drag, so `cursor` is only ever the AT-REST value.
  //
  // DELIBERATE non-re-arm on release — do NOT "fix": after a drag that ends
  // with the pointer still resting on the divider, no fresh `pointerenter`
  // fires (the pointer never crossed the boundary), so the resize cursor stays
  // disarmed until the user moves off and back on. Restoring it would require
  // re-arming on release/move via geometry or a retargeted `pointermove` — both
  // signals a STRANDED WebKit capture can forge, and the stranding is
  // undetectable (releasePointerCapture can silently fail without throwing) and
  // `pointerleave` may be withheld under it, so a re-armed cursor could never be
  // reliably cleared. That reintroduces the exact stuck-cursor bug this fix
  // exists to kill. A fresh, genuine `pointerenter` is the ONLY re-arm signal a
  // stranded capture cannot synthesize; the minor, self-healing affordance gap
  // is the price of stranding-safety (see the story's Risks & guardrails).
  const cursor = armed ? opts.dragCursor : undefined;

  return {
    onPointerDown,
    onPointerMove,
    onPointerUp,
    onPointerCancel,
    onPointerEnter,
    onPointerLeave,
    cursor,
  };
}
