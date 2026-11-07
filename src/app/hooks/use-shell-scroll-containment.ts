// SPDX-License-Identifier: MIT
import { useEffect } from "react";

/**
 * SW-FX-97: the app document must NEVER scroll — every scrollable surface is an
 * inner pane that owns its own overflow. SW-FX-88 pins the root with
 * `overflow: hidden`, but `overflow: hidden` leaves the root element
 * PROGRAMMATICALLY scrollable: a native focus-reveal (e.g. clicking a note in the
 * rail focuses the row, and the browser scrolls `document.scrollingElement` to
 * bring it into view), a `scrollIntoView`, or an overscroll chain can still move
 * the document's scroll offset. Because the root is clipped, that offset never
 * visually "reveals" anything — it just shoves the whole app shell up, leaving a
 * black band at the bottom that COMPOUNDS on every such interaction.
 *
 * Rather than hunt down every focus-reveal / scroll-into-view caller (there are
 * many, and new ones appear), enforce the invariant in ONE place: whenever the
 * document scrolls, snap it straight back to the origin. Inner panes are
 * unaffected — their scroll events leave `document.scrollingElement.scrollTop` at
 * 0, so the guard is a no-op for them; it only ever corrects the document itself.
 * Capture + passive so it observes the scroll early and never blocks it.
 */
export function useShellScrollContainment(): void {
  useEffect(() => {
    const pin = () => {
      const el = document.scrollingElement;
      if (el && (el.scrollTop !== 0 || el.scrollLeft !== 0)) {
        el.scrollTo(0, 0);
      }
    };
    window.addEventListener("scroll", pin, { capture: true, passive: true });
    return () => window.removeEventListener("scroll", pin, { capture: true });
  }, []);
}
