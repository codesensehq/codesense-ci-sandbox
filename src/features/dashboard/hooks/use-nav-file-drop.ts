// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { dropPositionHitsElement, type DragPosition } from "@/shared/lib/drag-drop-hit-test";
import { pickDropDir, type DropCandidate } from "@/features/dashboard/lib/nav-file-drop";

/** The currently-highlighted OS-drop target: a specific directory (its absolute
 *  path) or the nav root zone. `null` when no OS file drag is over this nav. */
export type NavDropHighlight = { dir: string } | { root: true } | null;

/**
 * SW-VE-7: OS file drag-drop onto the context nav. Tauri's `onDragDropEvent` is
 * webview-GLOBAL and delivers a position (not a DOM target), so — exactly like
 * `useTerminalDragDrop` — every mounted nav's listener fires on one drop and
 * must hit-test the position against its own scroll bounds before acting. Only
 * the nav under the cursor highlights + ingests; a drop outside it (e.g. on a
 * terminal tile) is ignored here, so the two webview-drop consumers never
 * cross-fire.
 *
 * Directory rows are marked in the DOM with `data-nav-drop-dir="<absPath>"`; the
 * hook reads those rects live during a drag (never at idle — only after the
 * cheap bounds gate) to resolve the target. A drop over any non-directory row or
 * empty nav space resolves to the root zone.
 */
export function useNavFileDrop(opts: {
  /** The nav scroll container (VirtualList) — its rect is the nav bounds and it
   *  hosts the `[data-nav-drop-dir]` row rects. */
  getScrollElement: () => HTMLElement | null;
  /** True while ingest is suppressed (e.g. a modal/picker is open) — read off a
   *  ref so the once-on-mount listener honors the live value. Optional. */
  disabledRef?: React.RefObject<boolean>;
  /** Fired on a drop inside the nav with the resolved target + OS file paths. */
  onDrop: (target: { dir: string } | { root: true }, paths: string[]) => void;
}): NavDropHighlight {
  const [highlight, setHighlight] = useState<NavDropHighlight>(null);
  const { getScrollElement: getEl, disabledRef, onDrop } = opts;
  // Keep the latest `onDrop` in a ref so the webview listener subscribes ONCE on
  // mount (mirrors `useTerminalDragDrop`). Putting `onDrop` in the effect deps
  // would re-subscribe on every render where its identity changes — and the
  // caller's `onDrop` depends on `tree`, which the post-ingest `bumpFileRefresh`
  // itself mutates — churning subscribe/unsubscribe and risking two overlapping
  // listeners (a double ingest) during the async unlisten window.
  const onDropRef = useRef(onDrop);
  useEffect(() => {
    onDropRef.current = onDrop;
  }, [onDrop]);

  useEffect(() => {
    const resolve = (position: DragPosition): NavDropHighlight => {
      const el = getEl();
      if (!el) return null;
      if (!dropPositionHitsElement(position, el)) return null; // not this nav
      const candidates: DropCandidate[] = Array.from(
        el.querySelectorAll<HTMLElement>("[data-nav-drop-dir]"),
      ).map((node) => ({ dir: node.dataset.navDropDir ?? "", rect: node.getBoundingClientRect() }));
      const dir = pickDropDir(position, candidates);
      return dir ? { dir } : { root: true };
    };

    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      if (disabledRef?.current) {
        setHighlight(null);
        return;
      }
      if (event.payload.type === "enter" || event.payload.type === "over") {
        // Assign the hit (not an unconditional value) so crossing rows re-lights
        // the destination and leaving the nav clears it.
        setHighlight(resolve(event.payload.position));
      } else if (event.payload.type === "leave") {
        setHighlight(null);
      } else if (event.payload.type === "drop") {
        const target = resolve(event.payload.position);
        setHighlight(null);
        if (!target) return; // drop landed outside this nav
        const paths = event.payload.paths;
        if (paths.length === 0) return;
        onDropRef.current(target, paths);
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
    // Deps are stable (memoized `getEl`, a ref object) → subscribe ONCE; the live
    // `onDrop` is read through `onDropRef` above.
  }, [getEl, disabledRef]);

  return highlight;
}
