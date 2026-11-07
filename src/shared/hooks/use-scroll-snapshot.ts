// SPDX-License-Identifier: MIT
import { useLayoutEffect, useRef } from "react";

import { useFocusWhenShown, useOwningScope } from "@/shared/lib/warm-scopes";
import { useViewerStore } from "@/shared/stores/viewer-store";

/**
 * RF-perf-12: persist a scroll container's `scrollTop` into the session-scoped
 * `viewStateByKey` slice across an unmount→remount, so RF-perf-10's team-unmount
 * / file-eviction no longer resets scroll to the top.
 *
 * Non-blocking by construction:
 *  - a PASSIVE `scroll` listener records `scrollTop` into a closure-local var
 *    (no re-render, no store write on every scroll event);
 *  - the last value is written to the store ONCE, in the SAME effect's cleanup
 *    (unmount / view change), O(1);
 *  - restore (opt-in) runs in a `useLayoutEffect` BEFORE paint, so there is no
 *    scroll-top flash.
 *
 * A warm team stays mounted but its box is hidden, and an engine drops a hidden box's scroll
 * offset: the tracked value is put back when the owning team is shown again.
 *
 * Reading `scrollTop` at unmount-cleanup time is unreliable (the node may be
 * detached), so we track it live via the listener and write the tracked value.
 *
 * Capture lives in the SAME effect as the listener (not a separate one) so the
 * `viewKey` and the tracked value are captured together in one closure. That
 * makes an in-place `viewKey` change (same component reused for a different key)
 * correct: the OLD effect's cleanup writes the OLD element's last scroll under
 * the OLD key BEFORE the new effect re-points at the new element.
 */
export function useScrollSnapshot(opts: {
  /** Composed session key (`navViewKey` / `fileViewKey`). Null disables. */
  viewKey: string | null | undefined;
  /** Returns the live scroll element (may be null before content mounts). */
  getEl: () => HTMLElement | null;
  /** Element is populated + measured — gates restore against a stale height. */
  ready: boolean;
  /** Apply the saved `scrollTop` once when `ready`. Default true. Markdown
   *  opts out (`false`) because it restores via its own async-render gate. */
  restore?: boolean;
}): void {
  const { viewKey, getEl, ready, restore = true } = opts;
  const getElRef = useRef(getEl);
  // One-shot restore latch, reset when the view changes (below) so a reused
  // component restores the NEW key but a `ready`-flip re-run doesn't clobber a
  // scroll the user has since moved.
  const restoredRef = useRef(false);
  const lastRef = useRef(0);

  useLayoutEffect(() => {
    getElRef.current = getEl;
  });

  // Reset the restore latch when the view changes — defined BEFORE the main
  // effect so its setup runs first on a `viewKey` change.
  useLayoutEffect(() => {
    restoredRef.current = false;
  }, [viewKey]);

  // Restore-once (pre-paint) + attach the passive scroll listener + capture on
  // teardown. Re-runs when `ready` flips so a virtualized/async list gets wired
  // after its rows land. NOTE: restore of an offset (not an anchor) can land at
  // a different item if the content height changed since capture — acceptable
  // per AC-1 ("± virtualizer rounding"); the felt goal is "roughly where I was".
  useLayoutEffect(() => {
    if (!viewKey || !ready) return;
    const el = getElRef.current();
    if (!el) return;
    if (restore && !restoredRef.current) {
      const saved = useViewerStore.getState().readViewState(viewKey)?.scrollTop;
      if (typeof saved === "number") el.scrollTop = saved;
      restoredRef.current = true;
    }
    lastRef.current = el.scrollTop;
    const onScroll = () => {
      lastRef.current = el.scrollTop;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      el.removeEventListener("scroll", onScroll);
      // `viewKey` + `last` are captured together in this closure, so the write
      // always targets the key the tracked value belongs to.
      useViewerStore.getState().captureViewState(viewKey, { scrollTop: lastRef.current });
    };
  }, [viewKey, ready, restore]);

  // The team was hidden and is shown again (not a first mount): the engine reset the offset.
  useFocusWhenShown(useOwningScope(), !!viewKey && ready, () => {
    const el = getElRef.current();
    if (el) el.scrollTop = lastRef.current;
  });
}
