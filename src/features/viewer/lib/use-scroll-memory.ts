// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef } from "react";

import {
  initScrollMemory,
  onVisibilityChange,
  recordScroll,
  resetScroll,
} from "./scratch-scroll-memory";

interface Options {
  /**
   * Re-apply the saved offset when the element (re)attaches. A container that
   * UNMOUNTS/REMOUNTS — the notes rail list lives in a Radix `<TabsContent>` that
   * Radix drops on a mode switch — never fires a `display:none`→visible edge: a
   * fresh `ResizeObserver` sees a non-zero size from the first frame. Arming
   * "hidden" on attach routes the first visible frame through the normal
   * hidden→visible restore, whether the element is visible now or only after
   * layout. The editor body never unmounts, so it leaves this off and its
   * behavior is unchanged (SLW-FX-2 AC-5/AC-7).
   */
  restoreOnAttach?: boolean;
}

/**
 * Scratch-note scroll memory across the right pane's minimize/restore (SLW-FX-2).
 * Owns the whole concern: the `ScrollMemory` state (the pure, tested decision
 * core in `scratch-scroll-memory.ts`) plus the DOM plumbing around it — a passive
 * `scroll` listener records the live offset while visible, and a `ResizeObserver`
 * detects the `display:none`↔visible flip (SW-RP-2: rendered size → 0 = hidden)
 * and restores `scrollTop` on the hidden→visible edge. Consumers never touch the
 * memory directly.
 *
 * Returns a stable `attach` ref-callback for the scroll element, and `resetToTop`
 * (forget the saved offset and scroll to the top — e.g. the editor on a note
 * change). Restores suppress the self-induced scroll event for one frame so a
 * restore never re-records itself.
 *
 * Consumed by the notes editor body (`scratch-note-panel.tsx`) and the notes rail
 * list (`notes-rail.tsx`) — one wiring, two scroll containers.
 */
export function useScrollMemory({ restoreOnAttach = false }: Options = {}): {
  attach: (el: HTMLElement | null) => void;
  resetToTop: () => void;
} {
  const memRef = useRef(initScrollMemory(true));
  const elRef = useRef<HTMLElement | null>(null);
  const cleanupRef = useRef<(() => void) | null>(null);
  const restoringRef = useRef(false);
  const rafRef = useRef(0);

  const applyScroll = useCallback((top: number) => {
    const el = elRef.current;
    if (!el) return;
    // True for one frame after WE assign scrollTop, so the scroll event our own
    // assignment fires doesn't overwrite the saved offset we're restoring to.
    restoringRef.current = true;
    el.scrollTop = top;
    cancelAnimationFrame(rafRef.current);
    rafRef.current = requestAnimationFrame(() => {
      restoringRef.current = false;
    });
  }, []);

  const attach = useCallback(
    (el: HTMLElement | null) => {
      // Detach any previous element first (element swap / remount).
      cleanupRef.current?.();
      cleanupRef.current = null;
      elRef.current = el;
      // Bail where ResizeObserver is absent (non-browser/test env) — scroll
      // restore is a real-DOM concern.
      if (!el || typeof ResizeObserver === "undefined") return;

      const isVisible = () => el.offsetHeight > 0;

      const onScroll = () => {
        if (restoringRef.current) return;
        memRef.current = recordScroll(memRef.current, isVisible(), el.scrollTop);
      };
      el.addEventListener("scroll", onScroll, { passive: true });

      // A fresh (re)mount fires no display flip. Arm "hidden" so the initial
      // ResizeObserver notification (or the first visible frame after layout)
      // takes the normal hidden→visible restore path (AC-7). Also suppress
      // recording until that restore lands: a stray scroll event in the window
      // before the RO callback would otherwise record the fresh element's 0 and
      // clobber `saved` — the RO restore (or a detach) clears the flag.
      if (restoreOnAttach) {
        memRef.current = { ...memRef.current, hidden: true };
        restoringRef.current = true;
      }

      const ro = new ResizeObserver(() => {
        const [next, restoreTo] = onVisibilityChange(memRef.current, isVisible());
        memRef.current = next;
        if (restoreTo !== null) applyScroll(restoreTo);
      });
      ro.observe(el);

      cleanupRef.current = () => {
        cancelAnimationFrame(rafRef.current);
        // Cancelling the rAF above strands `restoring` true if we detach mid-flight
        // (an applyScroll's clear-frame never runs), which would then suppress
        // recording forever on the next attach. `restoring` is per-element, so
        // clear it here — restoring the old inline effect's fresh-per-attach state.
        restoringRef.current = false;
        el.removeEventListener("scroll", onScroll);
        ro.disconnect();
      };
    },
    [applyScroll, restoreOnAttach],
  );

  // A new note starts at the top: forget the saved offset and scroll the live
  // container up (setContent alone leaves the old scrollTop, which the browser
  // keeps when the new note is at least as tall). Suppression keeps the induced
  // scroll event from re-recording a stale offset.
  const resetToTop = useCallback(() => {
    memRef.current = resetScroll(memRef.current);
    applyScroll(0);
  }, [applyScroll]);

  // Detach on unmount of the owning component (the ref callback also fires
  // attach(null) when the element itself unmounts, but the owner may outlive it).
  useEffect(() => () => cleanupRef.current?.(), []);

  return { attach, resetToTop };
}
