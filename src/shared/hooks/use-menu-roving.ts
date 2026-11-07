// SPDX-License-Identifier: MIT
import { useRef } from "react";

/** What the arrows land on: every enabled control in the menu, minus the mouse-only
 *  extras marked `data-roving-skip` (each has a keyboard path on its row instead). */
const ITEM_SELECTOR =
  "button:not(:disabled), input:not(:disabled), [role^='menuitem']:not([aria-disabled='true'])";

/** Type-ahead letters typed within this window build one prefix ("de" → "default"). */
const TYPEAHEAD_MS = 500;

/** The menu's focusable items under `root`, in document order. */
export function menuItems(root: ParentNode): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(ITEM_SELECTOR)).filter(
    (el) => !el.closest("[data-roving-skip]"),
  );
}

function itemLabel(el: HTMLElement): string {
  return (el.getAttribute("aria-label") ?? el.textContent ?? "").trim().toLowerCase();
}

function isTextField(el: HTMLElement): boolean {
  return el instanceof HTMLInputElement && el.type !== "checkbox" && el.type !== "radio";
}

/**
 * Roving focus for a menu whose rows are real controls (buttons, a switch, a form
 * field): ArrowUp/ArrowDown move focus and wrap, Home/End jump to the ends, typing
 * jumps to the next row starting with what you typed, and the pointer moves focus
 * with it so the mouse and keyboard highlight the same row. Esc and click-away
 * belong to the popover hosting the menu.
 *
 * DOM-driven rather than index-driven so a menu assembled from several sections
 * (each owning its own rows) needs no shared bookkeeping.
 */
export function useMenuRoving() {
  const typed = useRef({ text: "", at: 0 });

  const onKeyDown = (e: React.KeyboardEvent<HTMLElement>) => {
    const items = menuItems(e.currentTarget);
    if (items.length === 0) return;
    const target = e.target as HTMLElement;
    const inField = isTextField(target);
    const current = items.indexOf(document.activeElement as HTMLElement);
    const go = (index: number) => {
      e.preventDefault();
      items[(index + items.length) % items.length]?.focus();
    };

    if (e.key === "ArrowDown") return go(current < 0 ? 0 : current + 1);
    if (e.key === "ArrowUp") return go(current < 0 ? items.length - 1 : current - 1);
    if (inField) return;
    if (e.key === "Home") return go(0);
    if (e.key === "End") return go(items.length - 1);

    if (e.key.length !== 1 || e.key === " " || e.metaKey || e.ctrlKey || e.altKey) return;
    const now = Date.now();
    const prefix =
      (now - typed.current.at < TYPEAHEAD_MS ? typed.current.text : "") + e.key.toLowerCase();
    typed.current = { text: prefix, at: now };
    // A fresh letter searches from the row after the focused one, so repeating it cycles.
    const start = prefix.length === 1 ? current + 1 : Math.max(current, 0);
    const ordered = [...items.slice(start), ...items.slice(0, start)];
    const hit = ordered.find((el) => itemLabel(el).startsWith(prefix));
    if (hit) {
      e.preventDefault();
      hit.focus();
    }
  };

  const onPointerMove = (e: React.PointerEvent<HTMLElement>) => {
    const row = (e.target as HTMLElement).closest<HTMLElement>(ITEM_SELECTOR);
    if (!row || row === document.activeElement || row.closest("[data-roving-skip]")) return;
    if (isTextField(document.activeElement as HTMLElement)) return;
    if (e.currentTarget.contains(row)) row.focus({ preventScroll: true });
  };

  return { onKeyDown, onPointerMove };
}
