// SPDX-License-Identifier: MIT
import { useState, useCallback, type ReactNode } from "react";
import { AppButton as Button } from "@/shared/components/app-button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

interface RenameContentDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Current basename, used to prefill the input. Pass `""` for a "New file"
   *  prompt (empty field, confirm enabled for any non-empty name). */
  currentName: string;
  loading: boolean;
  /** Called with the new (trimmed) basename when the user confirms. */
  onConfirm: (newName: string) => void;
  /** SW-VE-6: optional copy so the same name-input dialog serves both Rename
   *  (default) and New file. */
  title?: string;
  description?: string;
  /** Confirm-button label (idle) and its loading variant. */
  confirmLabel?: string;
  loadingLabel?: string;
  /** Extra fields under the name input (New file's "Who can use it"). */
  children?: ReactNode;
}

/**
 * SW-CP-14: a minimal name-input prompt for the pipeline nav. A basename input
 * prefilled with `currentName`; Enter confirms. Path resolution + validation
 * live in `structural-ops.ts` (the caller); this is presentation. SW-VE-6
 * parametrised the copy so it also serves the "New file" gesture.
 */
export function RenameContentDialog({
  open,
  onOpenChange,
  currentName,
  loading,
  onConfirm,
  title = "Rename",
  description = "Rename this item in team content. Workspaces keep their copies until an agent hydrates them.",
  confirmLabel = "Rename",
  loadingLabel = "Renaming…",
  children,
}: RenameContentDialogProps) {
  // Initialized from `currentName`; the parent passes a `key` keyed on the
  // rename target so a new target remounts this with a fresh field (no
  // setState-in-effect reset needed).
  const [value, setValue] = useState(currentName);

  const submit = useCallback(() => {
    const trimmed = value.trim();
    if (trimmed && trimmed !== currentName) onConfirm(trimmed);
    else if (trimmed === currentName) onOpenChange(false);
  }, [value, currentName, onConfirm, onOpenChange]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        <input
          aria-label="File name"
          autoFocus
          value={value}
          disabled={loading}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              submit();
            }
          }}
          className="w-full rounded-md border border-[var(--border)] bg-[var(--background)] px-3 py-2 text-sm outline-none focus:border-[var(--ring)]"
        />
        {children}
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={loading}>
            Cancel
          </Button>
          <Button
            variant="default"
            onClick={submit}
            disabled={loading || !value.trim() || value.trim() === currentName}
          >
            {loading ? loadingLabel : confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
