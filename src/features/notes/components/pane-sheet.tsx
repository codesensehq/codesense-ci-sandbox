// SPDX-License-Identifier: MIT
import { useEffect, useRef, type ReactNode } from "react";

import { cn } from "@/lib/utils";

import { isInHiddenTeam } from "@/shared/lib/warm-scopes";
import { menuArrowKeys } from "./nt-popover";

/**
 * A float that spans the pane under its chrome row (routine setup, change history):
 * wide content that reads as part of the pane rather than a menu off a button.
 * Escape and a press outside close it; focus moves in on open and back on close.
 */
export function PaneSheet({
  label,
  top,
  onClose,
  className,
  children,
}: {
  label: string;
  /** Distance from the pane's top edge, in px. */
  top: number;
  onClose: () => void;
  className?: string;
  children: ReactNode;
}): React.JSX.Element {
  const ref = useRef<HTMLDivElement>(null);
  const closeRef = useRef(onClose);
  useEffect(() => {
    closeRef.current = onClose;
  });

  useEffect(() => {
    const el = ref.current;
    const before = document.activeElement as HTMLElement | null;
    el?.querySelector<HTMLElement>("[data-autofocus], button, select, textarea, input")?.focus();
    const onDown = (e: PointerEvent) => {
      const target = e.target as Element;
      if (el && isInHiddenTeam(el)) return;
      // A choice list opened from inside the sheet is part of it.
      if (el && !el.contains(target) && !target.closest?.(".nt-pop")) closeRef.current();
    };
    document.addEventListener("pointerdown", onDown, true);
    return () => {
      document.removeEventListener("pointerdown", onDown, true);
      if (before && document.contains(before)) before.focus({ preventScroll: true });
    };
  }, []);

  return (
    <div
      ref={ref}
      role="dialog"
      aria-label={label}
      className={cn("nt-pop nt-pane-sheet", className)}
      style={{ position: "absolute", top, left: 8, right: 8, maxWidth: "none" }}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          closeRef.current();
          return;
        }
        menuArrowKeys(e);
      }}
    >
      {children}
    </div>
  );
}
