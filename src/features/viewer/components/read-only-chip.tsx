// SPDX-License-Identifier: MIT
import { Lock, Pencil } from "lucide-react";

import { cn } from "@/lib/utils";
import { HEADER_BTN } from "./header-button";

/** SW-VE-16 Clean minimal / IDE-native: Read-only reads as a plain inline STATUS — amber
 *  color + icon + label, no bg, no border. The amber comes from the SW-IB1 `--ibx-amber`
 *  token (which resolves to the app's `--cs-status-modified`) so both themes stay in step.
 *  The badge is NOT a chip; it's a status word next to the Edit action. */
const RO_INLINE = "inline-flex h-[22px] items-center gap-1.5 px-1 text-[12px] font-medium";
const RO_INLINE_STYLE = {
  color: "var(--ibx-amber, var(--cs-status-modified))",
} as const;

interface ReadOnlyChipProps {
  className?: string;
  /** SW-VE-3: the file is in an own-writable scope, so the chip exposes a deliberate "Edit"
   *  action beside the "Read-only" status. When false (inherited / read-only-only) it stays
   *  the inert inline status. */
  canEdit?: boolean;
  /** SW-VE-3: edit mode is active for this file — the chip reads "Editing" with a dirty-dot
   *  slot. */
  editing?: boolean;
  /** SW-VE-3: the editing buffer has unsaved changes (drives the dirty dot). */
  dirty?: boolean;
  /** SW-VE-3: enter edit mode (only wired when `canEdit`). */
  onEdit?: () => void;
}

/**
 * SW-VE-2/3/16: the Read-only chip in the context-viewer toolbar. Read-only is the epic's
 * load-bearing default (agents author context; the human never accidentally edits) — the
 * chip makes that state VISIBLE.
 *
 * **SW-VE-16 Clean minimal / IDE-native** direction:
 * - **editing** → single "Editing" chip + dirty-dot (bordered neutral chrome).
 * - **canEdit** (own-writable scope) → inline amber "Read-only" **status** (no bg, no
 *   border — just colored text) + a bordered "Edit" **action button** with R3 fix 4's
 *   contrast step-up (1 px border + semibold so it carries primary-right-zone weight).
 *   The two pieces read as `status + action`; the status is the colored half, the button
 *   is the deliberate action with its own `--ibx-raise`-equivalent hover token.
 * - otherwise (inherited / read-only-only) → the inert amber status alone.
 *
 * SW-VE-3 semantics (prompt → unlock → save → relock) are unchanged — only chrome.
 *
 * SW-VE-8: content-agnostic — the caller renders it for ANY editable text file (markdown /
 * html / code). Lives in the context-viewer toolbar, NOT inside shared `renderViewer`.
 */
export function ReadOnlyChip({
  className,
  canEdit = false,
  editing = false,
  dirty = false,
  onEdit,
}: ReadOnlyChipProps) {
  if (editing) {
    return (
      <div className={className}>
        <span aria-label="Editing" className={cn(HEADER_BTN, "cursor-default text-foreground")}>
          <Pencil
            size={12}
            aria-hidden
            style={{ color: "var(--ibx-mfg, var(--muted-foreground))" }}
          />
          <span className="leading-none">Editing</span>
          {dirty && (
            <span
              data-testid="read-only-chip-dirty-dot"
              aria-label="Unsaved changes"
              className="size-1.5 shrink-0 rounded-full"
              // R4 N1: the dirty-dot is a load-bearing SW-VE-3 signal (the ONLY way the
              // user sees "your buffer isn't saved yet"). Use the full `--ibx-accent`, not
              // the 40%-transparent `--ibx-accent-line` meant for 1-px rings on opaque
              // surfaces — a dot at that opacity disappears against a bright chip bg.
              style={{ backgroundColor: "var(--ibx-accent)" }}
            />
          )}
        </span>
      </div>
    );
  }

  if (canEdit && onEdit) {
    return (
      <div className={cn("flex items-center gap-1.5", className)}>
        <span aria-label="Read-only" className={RO_INLINE} style={RO_INLINE_STYLE}>
          <Lock size={12} aria-hidden />
          <span className="leading-none">Read-only</span>
        </span>
        <button
          type="button"
          aria-label="Edit"
          onClick={onEdit}
          className={cn(HEADER_BTN, "font-semibold text-foreground")}
          data-testid="read-only-chip-edit"
        >
          <Pencil
            size={12}
            aria-hidden
            style={{ color: "var(--ibx-mfg, var(--muted-foreground))" }}
          />
          <span className="leading-none">Edit</span>
        </button>
      </div>
    );
  }

  return (
    <div className={className}>
      <span aria-label="Read-only" className={RO_INLINE} style={RO_INLINE_STYLE}>
        <Lock size={12} aria-hidden />
        <span className="leading-none">Read-only</span>
      </span>
    </div>
  );
}
