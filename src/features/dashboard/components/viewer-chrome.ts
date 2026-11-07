// SPDX-License-Identifier: MIT
import { DEFAULT_APP_THEME } from "@/themes";
import { useEffect, type RefObject } from "react";

import { getEffectiveViewerFontPx } from "@/shared/lib/font-sizes";
import { isInHiddenTeam } from "@/shared/lib/warm-scopes";
import { getActivePaneType } from "@/shared/lib/focus-utils";
import { parseScrollIntent } from "@/shared/lib/scroll-keys";
import { useViewerStore } from "@/shared/stores/viewer-store";

/**
 * Shared viewer chrome — the scroll container styling, content-width / theme /
 * font settings, and keyboard scroll shortcuts that EVERY rendered-content
 * viewer (markdown, and the SW-GS-3 redline) shares. Extracted from
 * `markdown-viewer.tsx` so the redline viewer inherits identical hotkeys + look
 * without re-deriving them (and so there is one source of truth).
 */

export const SCROLL_CONTAINER_STYLE = { overflowAnchor: "auto" } as const;

export const WIDTH_MAP: Record<string, string> = {
  narrow: "max-w-[680px]",
  normal: "max-w-[780px]",
  wide: "max-w-[860px]",
  full: "max-w-none",
};

export interface ViewerChrome {
  /** Tailwind max-width class for the content column. */
  widthClass: string;
  /** True when a non-default viewer theme is active. */
  hasTheme: boolean;
  /** Resolved prose font size, e.g. "18px". */
  fontSize: string;
}

/** Read the viewer content-width / theme / font settings and derive the
 *  presentation values markdown + redline viewers apply identically. */
export function useViewerChrome(): ViewerChrome {
  const contentWidth = useViewerStore((s) => s.settings?.contentWidth) ?? "normal";
  const appTheme = useViewerStore((s) => s.settings?.appTheme) ?? DEFAULT_APP_THEME;
  const viewerFontSize = useViewerStore((s) => s.settings?.viewerFontSize) ?? "large";
  const viewerFontSizePx = useViewerStore((s) => s.settings?.viewerFontSizePx);
  return {
    widthClass: WIDTH_MAP[contentWidth] ?? WIDTH_MAP.normal,
    hasTheme: appTheme !== DEFAULT_APP_THEME,
    fontSize: `${getEffectiveViewerFontPx(viewerFontSize, viewerFontSizePx)}px`,
  };
}

/**
 * Keyboard scroll shortcuts for the active file viewer, bound to its scroll
 * container. Skips when focus is in an input/terminal/scratch-note, and only
 * the currently-visible keep-alive viewer reacts (`offsetParent` gate).
 *
 *   ArrowUp / ArrowDown          — scroll by one line
 *   Alt+ArrowUp / Alt+ArrowDown  — scroll by 10x line step (chunk)
 *   PageUp / PageDown            — scroll by ~one viewport
 *   Home / End                   — scroll to top / bottom
 *   Cmd+ArrowUp / Cmd+ArrowDown  — scroll to top / bottom (mac convention)
 *   Cmd+A                        — select the prose contents only (scoped)
 */
export function useViewerScrollKeys(
  scrollRef: RefObject<HTMLDivElement | null>,
  proseRef: RefObject<HTMLDivElement | null>,
): void {
  useEffect(() => {
    const LINE_STEP = 60;
    const handler = (e: KeyboardEvent) => {
      const active = document.activeElement as HTMLElement | null;
      const inInput =
        !!active &&
        (active.tagName === "INPUT" || active.tagName === "TEXTAREA" || active.isContentEditable);
      if (inInput) return;
      if (getActivePaneType() !== "file") return;
      if (scrollRef.current && isInHiddenTeam(scrollRef.current)) return;

      const cmd = e.metaKey || e.ctrlKey;

      // Cmd+A owns the keystroke whenever the viewer pane has focus.
      if (cmd && !e.shiftKey && !e.altKey && (e.key === "a" || e.key === "A")) {
        e.preventDefault();
        const prose = proseRef.current;
        if (!prose || prose.offsetParent === null) return;
        const range = document.createRange();
        range.selectNodeContents(prose);
        const selection = window.getSelection();
        selection?.removeAllRanges();
        selection?.addRange(range);
        return;
      }

      const el = scrollRef.current;
      if (!el || el.offsetParent === null) return;

      // Let native "extend selection" keystrokes (Shift+…) pass through.
      if (e.shiftKey) return;

      const intent = parseScrollIntent(e);
      if (intent) {
        e.preventDefault();
        const pageStep = Math.max(el.clientHeight - 80, 200);
        switch (intent.action) {
          case "top":
            el.scrollTo({ top: 0, behavior: "smooth" });
            break;
          case "bottom":
            el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
            break;
          case "lines":
            el.scrollTo({ top: el.scrollTop + intent.delta * LINE_STEP, behavior: "smooth" });
            break;
          case "pages":
            el.scrollTo({ top: el.scrollTop + intent.delta * pageStep, behavior: "smooth" });
            break;
        }
        return;
      }

      // Local fallback: plain arrows (line scroll) and Home/End (top/bottom).
      let top: number | null = null;
      if (!cmd && !e.altKey && e.key === "ArrowUp") top = el.scrollTop - LINE_STEP;
      else if (!cmd && !e.altKey && e.key === "ArrowDown") top = el.scrollTop + LINE_STEP;
      else if (!cmd && e.key === "Home") top = 0;
      else if (!cmd && e.key === "End") top = el.scrollHeight;

      if (top !== null) {
        e.preventDefault();
        el.scrollTo({ top, behavior: "smooth" });
      }
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [scrollRef, proseRef]);
}
