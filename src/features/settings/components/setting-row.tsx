// SPDX-License-Identifier: MIT
import { useEffect, useRef, type ReactNode } from "react";

import { useUiStore } from "@/shared/stores/ui-store";

/** The nearest ancestor that scrolls vertically, if any. */
function scrollParent(el: HTMLElement): HTMLElement | null {
  for (let e = el.parentElement; e; e = e.parentElement) {
    const { overflowY } = getComputedStyle(e);
    if ((overflowY === "auto" || overflowY === "scroll") && e.scrollHeight > e.clientHeight)
      return e;
  }
  return null;
}

interface SettingRowProps {
  label: string;
  children: ReactNode;
  /** The name an opener uses to land on this row (`openSettingsTab(section, target)`). */
  target?: string;
}

/**
 * Horizontal setting row: label on the left, control on the right.
 * Used by every row in the app-wide Settings panel.
 *
 * Uses `flex items-start` (not `items-center`) so multi-line controls
 * (button grids, inline editors) align their first row to the label
 * and grow downward instead of pushing the label off-center.
 *
 * The label is a `<span>` (not `<label htmlFor=...>`): the controls
 * below are radiogroups / button grids whose accessible names come
 * from `aria-label` / `aria-labelledby` on the group itself, not from
 * a single form-control association.
 *
 * With a `target`, a Settings open that names it scrolls the row into view and focuses it
 * once it shows, then forgets the request.
 */
export function SettingRow({ label, children, target }: SettingRowProps) {
  const ref = useRef<HTMLDivElement>(null);
  const wanted = useUiStore((s) => target !== undefined && s.settingsInitialRow === target);
  useEffect(() => {
    const row = ref.current;
    if (!wanted || !row) return;
    // Scroll only the panel's own scroller, so the Settings header stays put.
    const scroller = scrollParent(row);
    if (scroller) {
      const offset = row.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
      scroller.scrollTo?.({ top: scroller.scrollTop + offset });
    } else row.scrollIntoView?.({ block: "nearest" });
    row.focus({ preventScroll: true });
    useUiStore.setState({ settingsInitialRow: null });
  }, [wanted]);
  return (
    <div
      ref={ref}
      data-setting-row={target}
      role={target ? "group" : undefined}
      tabIndex={target ? -1 : undefined}
      aria-label={target ? label : undefined}
      className="flex items-start gap-6 border-b border-border py-4 outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      <span className="w-40 shrink-0 pt-1 text-sm font-medium text-foreground">{label}</span>
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}
