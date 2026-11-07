// SPDX-License-Identifier: MIT
import type { LucideIcon } from "lucide-react";

import { InfoTip } from "@/components/ui/tooltip";

/**
 * SW-PP-3: the shared icon-only chrome button for the pipeline surface — the one
 * primitive behind the header toolbar controls (fit-all, and SW-PP-2/4/5/7's
 * buttons) AND the on-canvas overlay buttons (drill-out, focus-current). Extracts
 * the button chrome those had each hand-rolled identically: transparent, size-14
 * lucide icon, `text-muted-foreground` → `hover:text-foreground`, `p-1.5`.
 *
 * Disabled uses `aria-disabled` (not the native `disabled` attribute) so the
 * control stays announced and keeps its tooltip while dimmed, and click/keyboard
 * activation is suppressed by dropping the handler + `tabIndex={-1}` — the
 * accessible pattern focus-current already used. `stopPropagation` keeps a click
 * from bubbling to the right-pane section header's collapse/drag handler.
 *
 * SW-FX-80: the `title` renders through the shared Radix `InfoTip` (label) —
 * `aria-label` remains the accessible name, and the disabled state still reveals
 * the tip on hover because `aria-disabled` keeps pointer events firing (native
 * `disabled` would swallow them).
 */
export function ChromeIconButton({
  icon: Icon,
  title,
  onClick,
  disabled,
}: {
  icon: LucideIcon;
  title: string;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <InfoTip label={title}>
      <button
        type="button"
        aria-label={title}
        aria-disabled={disabled || undefined}
        tabIndex={disabled ? -1 : 0}
        onClick={
          disabled
            ? undefined
            : (e) => {
                e.stopPropagation();
                onClick();
              }
        }
        className={`p-1.5 text-muted-foreground transition-colors ${
          disabled ? "opacity-40 cursor-default" : "hover:text-foreground"
        }`}
        style={{ background: "transparent", border: "none" }}
      >
        <Icon size={14} />
      </button>
    </InfoTip>
  );
}
