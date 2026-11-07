// SPDX-License-Identifier: MIT
import { useCallback, useState } from "react";
import { MoreHorizontal } from "lucide-react";

import { FloatingMenu } from "@/shared/components/floating-menu";
import type { Context } from "@/shared/lib/context-model";
import type { ContextMobilityOp } from "@/shared/lib/move-gate-ladder";

/**
 * SLW-PT-16 — the per-file "Move to… / Copy to…" ENTRY POINT (§E). The ⋯ action
 * row on a file-tree / nav item that opens the org-scoped target picker. The
 * SOURCE context is already known (the item lives in the active context) — the
 * picker never asks which team you're demoting from.
 *
 * This is the thin trigger; it hands the chosen verb up to the caller, which
 * owns the target sheet + the SLW-PT-12 MoveSheet orchestration (source known,
 * org switch, resume). Keeping the trigger separate keeps it a pure, testable
 * affordance that can mount in any nav row (⋯ menu or a right-click surface).
 */
export interface MoveActionMenuProps {
  /** The item's source context (already known — never asked). */
  source: Context;
  /** The item's content-root-relative logical name + display name. */
  logicalName: string;
  fileName: string;
  /** Start the target picker for the chosen verb. */
  onStart: (op: ContextMobilityOp) => void;
  /** SLW-PT-14 — start the REMOVE (delete-in-place) flow. When omitted the
   *  Remove item is not shown, so the menu degrades cleanly where remove isn't
   *  offered. */
  onRemove?: () => void;
  /** Optional label for the trigger's aria-label ("More actions for <file>"). */
  triggerLabel?: string;
}

export function MoveActionMenu(props: MoveActionMenuProps): React.JSX.Element {
  const [open, setOpen] = useState(false);

  const start = useCallback(
    (op: ContextMobilityOp) => {
      setOpen(false);
      props.onStart(op);
    },
    [props],
  );

  const remove = useCallback(() => {
    setOpen(false);
    props.onRemove?.();
  }, [props]);

  return (
    <FloatingMenu
      open={open}
      onOpenChange={setOpen}
      role="menu"
      trigger={
        <button
          type="button"
          data-testid="move-action-trigger"
          aria-haspopup="menu"
          aria-expanded={open}
          aria-label={props.triggerLabel ?? `More actions for ${props.fileName}`}
          onClick={(e) => e.stopPropagation()}
          className="flex items-center rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-accent-foreground"
        >
          <MoreHorizontal size={14} aria-hidden="true" />
        </button>
      }
      style={{ minWidth: 160 }}
    >
      <button
        type="button"
        role="menuitem"
        data-testid="move-action-move"
        className="flex w-full items-center rounded px-2 py-1.5 text-left text-sm hover:bg-accent"
        onClick={(e) => {
          e.stopPropagation();
          start("move");
        }}
      >
        Move to…
      </button>
      <button
        type="button"
        role="menuitem"
        data-testid="move-action-copy"
        className="flex w-full items-center rounded px-2 py-1.5 text-left text-sm hover:bg-accent"
        onClick={(e) => {
          e.stopPropagation();
          start("copy");
        }}
      >
        Copy to…
      </button>
      {props.onRemove && (
        <button
          type="button"
          role="menuitem"
          data-testid="move-action-remove"
          className="flex w-full items-center rounded px-2 py-1.5 text-left text-sm text-destructive hover:bg-accent"
          onClick={(e) => {
            e.stopPropagation();
            remove();
          }}
        >
          Remove…
        </button>
      )}
    </FloatingMenu>
  );
}
