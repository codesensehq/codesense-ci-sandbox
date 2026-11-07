// SPDX-License-Identifier: MIT
import { useLayoutEffect, useState } from "react";

import { isTerminalSplitShown } from "@/features/viewer/lib/terminal-column";
import { useTerminalSection } from "@/features/viewer/lib/terminal-section-registry";
import { useViewerStore } from "@/shared/stores/viewer-store";

/** The full-screen inbox never gets shorter than this, however tall the terminal is. */
export const MIN_INBOX_PX = 200;
/** The title bar's height (h-10) before it is measured; the inbox starts below it. */
const TITLE_BAR_FALLBACK_PX = 40;
const TITLE_BAR_SELECTOR = ".titlebar-drag";

/**
 * How far above the window bottom the CLI's terminal split sits, in px, while
 * the terminal shows as a split below the viewer; 0 when it is hidden or fills the
 * column (console). The full-screen inbox stops this far short of the bottom so the
 * split divider and the CLI below it stay visible and usable — except under a terminal
 * so tall the inbox would drop below `MIN_INBOX_PX`, where the inbox keeps that height
 * and covers the divider and the CLI's top rows.
 *
 * Whether the split shows comes from the viewer store (the rule the workbench sizes
 * the section by); where it lands is a rendered fact (the split ratio is a fraction
 * of a column whose box only the DOM knows). The workbench registers the terminal
 * section, so this re-measures when the section mounts or is replaced, and when the
 * divider, the section, their column or the window resizes.
 */
export function useTerminalSplitInset(): number {
  const split = useViewerStore(isTerminalSplitShown);
  const section = useTerminalSection();
  const [inset, setInset] = useState(0);
  const active = split && section !== undefined;

  useLayoutEffect(() => {
    if (!active || !section) return;
    const observer =
      typeof ResizeObserver !== "undefined" ? new ResizeObserver(() => measure()) : null;
    const titleBar = document.querySelector<HTMLElement>(TITLE_BAR_SELECTOR);
    for (const el of [section, section.parentElement, titleBar]) {
      if (el) observer?.observe(el);
    }
    // The divider is re-resolved on every measure (it can be replaced under the
    // same column); the observer follows whichever one is current.
    let divider: HTMLElement | null = null;
    const measure = () => {
      const current = dividerAbove(section);
      if (current !== divider) {
        if (divider) observer?.unobserve(divider);
        if (current) observer?.observe(current);
        divider = current;
      }
      setInset(measureInset(section, current));
    };
    measure();
    window.addEventListener("resize", measure);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [active, section]);

  return active ? inset : 0;
}

/** The split divider the workbench renders above the terminal section, in the same
 *  column (an sr-only label may sit between them, so not just the previous sibling). */
function dividerAbove(section: HTMLElement): HTMLElement | null {
  return (
    section.parentElement?.querySelector<HTMLElement>(
      ":scope > [data-testid='terminal-drag-handle']",
    ) ?? null
  );
}

function measureInset(section: HTMLElement, divider: HTMLElement | null): number {
  const rect = section.getBoundingClientRect();
  // Not laid out (a hidden team, a zero-height section): nothing to leave uncovered.
  if (rect.height === 0) return 0;
  // A divider that is not laid out (no box) says nothing: use the section's top.
  const d = divider?.getBoundingClientRect();
  const top = d && d.height > 0 && d.width > 0 ? Math.min(rect.top, d.top) : rect.top;
  const titleBar = document.querySelector<HTMLElement>(TITLE_BAR_SELECTOR);
  const titleBarBottom = titleBar ? titleBar.getBoundingClientRect().bottom : TITLE_BAR_FALLBACK_PX;
  const max = Math.max(0, window.innerHeight - titleBarBottom - MIN_INBOX_PX);
  return Math.min(max, Math.max(0, Math.round(window.innerHeight - top)));
}
