// SPDX-License-Identifier: MIT
import { useCallback, useRef, type KeyboardEvent as ReactKeyboardEvent } from "react";

/**
 * Roving-tabindex keyboard behaviour for a horizontal tablist, shared by every in-panel tab switch so
 * their a11y can't drift. Arrow keys move to the previous/next tab (wrapping), Home/End jump to the
 * ends, and the newly-selected tab receives DOM focus. Selection state stays owned by the caller — this
 * hook only maps a key event to the next id, moves focus, and hands back the per-tab ARIA/roving props.
 *
 * `ids` must be a stable, ordered array (a module constant) — one entry per tab, in visual order.
 */
export function useRovingTabs<Id extends string>(
  ids: readonly Id[],
  selected: Id,
  onSelect: (id: Id) => void,
): {
  tabProps: (id: Id) => {
    ref: (element: HTMLButtonElement | null) => void;
    role: "tab";
    "aria-selected": boolean;
    tabIndex: 0 | -1;
    onKeyDown: (event: ReactKeyboardEvent<HTMLButtonElement>) => void;
  };
} {
  const refs = useRef(new Map<Id, HTMLButtonElement | null>());

  const onKeyDown = useCallback(
    (event: ReactKeyboardEvent<HTMLButtonElement>) => {
      const index = ids.indexOf(selected);
      if (index === -1) return;
      let next: Id | null = null;
      if (event.key === "ArrowRight") next = ids[(index + 1) % ids.length];
      else if (event.key === "ArrowLeft") next = ids[(index - 1 + ids.length) % ids.length];
      else if (event.key === "Home") next = ids[0];
      else if (event.key === "End") next = ids[ids.length - 1];
      if (next === null) return;
      event.preventDefault();
      onSelect(next);
      refs.current.get(next)?.focus();
    },
    [ids, selected, onSelect],
  );

  const tabProps = useCallback(
    (id: Id) => ({
      ref: (element: HTMLButtonElement | null) => {
        // Clear on unmount (React calls the ref with null) so a removed tab leaves no stale node.
        if (element) refs.current.set(id, element);
        else refs.current.delete(id);
      },
      role: "tab" as const,
      "aria-selected": selected === id,
      tabIndex: (selected === id ? 0 : -1) as 0 | -1,
      onKeyDown,
    }),
    [selected, onKeyDown],
  );

  return { tabProps };
}
