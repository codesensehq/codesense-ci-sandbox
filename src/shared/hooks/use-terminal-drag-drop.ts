// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { shellEscape, stripControlChars } from "@/shared/lib/shell-escape";
import { dropPositionHitsElement } from "@/shared/lib/drag-drop-hit-test";

// Re-exported so existing importers (and this hook's tests) keep resolving it
// here; the implementation now lives in the shared, dependency-free hit-test lib
// so the SW-VE-7 nav drop can reuse it without importing this React hook.
export { dropPositionInRect } from "@/shared/lib/drag-drop-hit-test";

/**
 * Build the terminal input for a set of dropped file paths: each path is
 * stripped of control chars, shell-escaped, and space-joined.
 *
 * Bracketed-paste framing is deliberately NOT applied here. The caller pastes
 * this through `term.paste()`, which frames (`\e[200~ … \e[201~`) only when the
 * terminal's live `bracketedPasteMode` is on — so a foreground program with
 * bracketed paste OFF (a raw `read`, `cat`, a REPL that never opted in) receives
 * the bare paths instead of literal `\e[200~` marker bytes.
 */
export function buildDropText(paths: string[]): string {
  return paths.map((p) => shellEscape(stripControlChars(p))).join(" ");
}

/**
 * Manages file drag-and-drop from Finder into a terminal panel. Dropped file
 * paths are sanitized, shell-escaped, and handed to `pasteText` — the caller
 * routes that through xterm's `paste()`, the same seam the right-click Paste
 * uses, so bracketed-paste framing and the `onData` input-lock gate are honored
 * once, in one place, rather than re-implemented here.
 *
 * The underlying `onDragDropEvent` is webview-GLOBAL, so every mounted+visible
 * terminal's listener fires on a single drop. `tileRef` is this terminal's
 * on-screen element: the drop `position` is hit-tested against its rect so only
 * the tile under the cursor pastes (and only it highlights) — without this, a
 * drop with N terminals visible fans out to all N (SW-EW-26). The element drawn
 * topmost at that point must also be inside the tile, so a popup or a maximized
 * overlay covering the tile takes the drop instead: the tile neither pastes nor
 * highlights under it.
 *
 * Returns `isDragOver` for visual feedback (CSS class toggle).
 */
export function useTerminalDragDrop(
  pasteText: (text: string) => void,
  visibleRef: React.RefObject<boolean>,
  disposedRef: React.RefObject<boolean>,
  tileRef: React.RefObject<HTMLElement | null>,
  /**
   * SW-GS-1 AC-11: when set and true, the terminal is input-locked (the agent
   * is driving) — suppress the drop highlight and skip the paste. `term.paste()`
   * would also drop the input at the `onData` gate, but gating here keeps the
   * tile from lighting up as a drop target it will not accept. Ref-backed (not a
   * captured boolean) because this listener closure is created once on mount; a
   * stale boolean would leak. Optional → existing callers (unlocked) unaffected.
   */
  inputLockedRef?: React.RefObject<boolean>,
): boolean {
  const [isDragOver, setIsDragOver] = useState(false);

  useEffect(() => {
    // True only when this terminal is visible, the drag position lands inside this
    // tile's rect, AND nothing else is drawn over the tile there (a quick view, an
    // inbox chat, a maximized board). Reads layout only while a drag is in flight,
    // and only after the cheap visible gate — no idle/steady-state cost.
    const positionHitsThisTile = (position: { x: number; y: number }): boolean => {
      if (!visibleRef.current) return false;
      const el = tileRef.current;
      if (!el) return false;
      return dropPositionHitsElement(position, el);
    };

    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      if (disposedRef.current) return;
      // SW-GS-1 AC-11: input-locked → no highlight, no paste. Read live off the
      // ref so the once-on-mount closure honors the current lock.
      if (inputLockedRef?.current) {
        setIsDragOver(false);
        return;
      }
      if (event.payload.type === "enter" || event.payload.type === "over") {
        // Assign the hit (not an unconditional true) so dragging from tile A onto
        // tile B clears A's highlight and lights B's as the cursor crosses.
        setIsDragOver(positionHitsThisTile(event.payload.position));
      } else if (event.payload.type === "leave") {
        setIsDragOver(false);
      } else if (event.payload.type === "drop") {
        setIsDragOver(false);
        if (!positionHitsThisTile(event.payload.position)) return;
        const paths = event.payload.paths;
        if (paths.length === 0) return;
        pasteText(buildDropText(paths));
      }
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [pasteText, visibleRef, disposedRef, tileRef, inputLockedRef]);

  return isDragOver;
}
