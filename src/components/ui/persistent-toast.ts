// SPDX-License-Identifier: MIT
import type { ExternalToast } from "sonner";

import { TOAST_BOTTOM_GAP_PX } from "./toaster-placement";

/**
 * Toasts that never expire on their own (an update, a prompt awaiting an
 * answer). Left full size they would sit over the bottom-right terminal's last
 * rows — its prompt, a TUI's input — for as long as they wait. So a persistent
 * toast collapses to a one-line chip (its title) after a few seconds, and opens
 * again while the pointer is on it or keyboard focus is inside it.
 */

/** Marks a toast as persistent; the toaster collapses it. */
export const PERSISTENT_TOAST_CLASS = "cs-toast-persistent";
/** Set on a persistent toast while it shows as a chip. */
export const TOAST_COLLAPSED_ATTR = "data-cs-collapsed";
/** Marks a custom toast's body, so its chip keeps only the first line. */
export const TOAST_BODY_ATTR = "data-cs-toast-body";
/** How long a persistent toast shows in full before it collapses. */
export const PERSISTENT_TOAST_COLLAPSE_MS = 6_000;
/** The chip's height. */
export const TOAST_CHIP_HEIGHT_PX = 18;
/** How far a persistent toast drops, in both states: the toaster's bottom gap,
 *  so it sits right on the bottom chrome and opens in place. */
export const PERSISTENT_TOAST_DROP_PX = TOAST_BOTTOM_GAP_PX;

/**
 * A collapsed toast — except while sonner measures it. Sonner re-measures a
 * toast whose content changes by setting its inline height to `auto` and
 * reading its box; the chip's clamp must not apply then, or sonner stores the
 * chip's height as the toast's and clips it when it opens again. Matched on the
 * `height` property alone, never sonner's `--initial-height: auto`.
 */
export const COLLAPSED_TOAST_SELECTOR = `[data-sonner-toast][${TOAST_COLLAPSED_ATTR}]:not([style^="height: auto"]):not([style*=" height: auto"])`;

/** Hidden from sight but still in the page: no live-region re-announce, and a
 *  screen reader's virtual cursor still reaches the buttons. */
const VISUALLY_HIDDEN = `position:absolute!important;width:1px!important;height:1px!important;margin:-1px!important;padding:0!important;border:0!important;overflow:hidden!important;clip-path:inset(50%)!important;white-space:nowrap!important;`;

/**
 * The chip, drawn from the constants above. A persistent toast sits on the
 * bottom chrome in both states (the bottom gap is taken back), so it opens in
 * place; collapsed, it is one {@link TOAST_CHIP_HEIGHT_PX} line as wide as its
 * title, covering no more than a terminal's last row.
 */
export function persistentToastCss(): string {
  const c = COLLAPSED_TOAST_SELECTOR;
  const line = TOAST_CHIP_HEIGHT_PX - 2; // inside a 1px border
  return `
[data-sonner-toast][data-y-position="bottom"].${PERSISTENT_TOAST_CLASS} { translate: 0 ${PERSISTENT_TOAST_DROP_PX}px; }
${c} {
  max-height: ${TOAST_CHIP_HEIGHT_PX}px !important;
  overflow: hidden;
  width: auto !important;
  max-width: var(--width);
  padding: 0 8px !important;
  gap: 4px !important;
  align-items: center;
}
${c} :is([data-description], [data-button], [data-close-button]),
${c} [${TOAST_BODY_ATTR}] > :not(:first-child) { ${VISUALLY_HIDDEN} }
${c} [data-icon] { height: 12px; width: 12px; }
${c} [data-content] { gap: 0; }
${c} :is([data-title], [${TOAST_BODY_ATTR}] > :first-child) {
  font-size: 11px;
  line-height: ${line}px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
${c} [${TOAST_BODY_ATTR}] {
  width: auto !important;
  padding: 0 8px !important;
  gap: 0 !important;
}
`;
}

/** Stamped on each issue of a persistent toast, so issuing it again (the same
 *  id) is told apart from its content changing (a download's progress). */
const ISSUE_ATTR_PREFIX = "cs-toast-issue-";
let issues = 0;

/** Options for a toast that waits until it is answered or dismissed. Each call
 *  is a new issue: a toast issued again under its id opens again. */
export function persistent(options: ExternalToast): ExternalToast {
  const classes = [options.className, PERSISTENT_TOAST_CLASS, `${ISSUE_ATTR_PREFIX}${++issues}`];
  return {
    ...options,
    duration: Number.POSITIVE_INFINITY,
    className: classes.filter(Boolean).join(" "),
  };
}

function issueOf(el: Element): string | undefined {
  return [...el.classList].find((c) => c.startsWith(ISSUE_ATTR_PREFIX));
}

/**
 * Collapses each persistent toast that appears under `host` after `delayMs`,
 * re-opens it on hover or focus, and collapses it again `delayMs` after the
 * pointer and focus have left. A toast issued again under its id is news again:
 * it opens and its clock restarts. Its content changing in place (progress)
 * leaves it as it is. Returns the teardown.
 */
export function collapsePersistentToasts(
  host: HTMLElement,
  delayMs = PERSISTENT_TOAST_COLLAPSE_MS,
): () => void {
  const teardowns = new Map<Element, () => void>();

  const arm = (el: HTMLElement) => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    let hovered = false;
    const schedule = () => {
      clearTimeout(timer);
      timer = setTimeout(() => el.setAttribute(TOAST_COLLAPSED_ATTR, ""), delayMs);
    };
    const open = () => {
      clearTimeout(timer);
      el.removeAttribute(TOAST_COLLAPSED_ATTR);
    };
    const onEnter = () => {
      hovered = true;
      open();
    };
    const onLeave = () => {
      hovered = false;
      if (!el.contains(document.activeElement)) schedule();
    };
    const onFocusOut = (e: FocusEvent) => {
      if (!hovered && !el.contains(e.relatedTarget as Node | null)) schedule();
    };
    el.addEventListener("mouseenter", onEnter);
    el.addEventListener("mouseleave", onLeave);
    el.addEventListener("focusin", open);
    el.addEventListener("focusout", onFocusOut);
    let issue = issueOf(el);
    const reissued = new MutationObserver(() => {
      const now = issueOf(el);
      if (now === issue) return;
      issue = now;
      open();
      if (!hovered && !el.contains(document.activeElement)) schedule();
    });
    reissued.observe(el, { attributes: true, attributeFilter: ["class"] });
    schedule();
    teardowns.set(el, () => {
      clearTimeout(timer);
      reissued.disconnect();
      el.removeEventListener("mouseenter", onEnter);
      el.removeEventListener("mouseleave", onLeave);
      el.removeEventListener("focusin", open);
      el.removeEventListener("focusout", onFocusOut);
    });
  };

  const sync = () => {
    for (const el of host.querySelectorAll<HTMLElement>(`.${PERSISTENT_TOAST_CLASS}`)) {
      if (!teardowns.has(el)) arm(el);
    }
    for (const [el, teardown] of teardowns) {
      if (!host.contains(el)) {
        teardown();
        teardowns.delete(el);
      }
    }
  };

  sync();
  const observer = new MutationObserver(sync);
  observer.observe(host, {
    childList: true,
    subtree: true,
    attributes: true,
    attributeFilter: ["class"],
  });
  return () => {
    observer.disconnect();
    for (const teardown of teardowns.values()) teardown();
    teardowns.clear();
  };
}
