// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";

import { AppButton as Button } from "@/shared/components/app-button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

interface ConfirmActionDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: string;
  confirmLabel: string;
  loadingLabel: string;
  onConfirm: () => void;
  loading: boolean;
  variant?: "destructive" | "default";
  /** Stable hook for driving the dialog from tests; omitted = no test id. */
  testId?: string;
  /** What the action takes with it (e.g. a list), between the description and the buttons. */
  children?: ReactNode;
}

export function ConfirmActionDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel,
  loadingLabel,
  onConfirm,
  loading,
  variant = "destructive",
  testId,
  children,
}: ConfirmActionDialogProps) {
  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        if (!nextOpen && loading) return;
        onOpenChange(nextOpen);
      }}
    >
      <DialogContent data-testid={testId}>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        {children}
        <DialogFooter>
          <Button
            variant="outline"
            autoFocus
            onClick={() => onOpenChange(false)}
            disabled={loading}
            data-testid={testId ? `${testId}-cancel` : undefined}
          >
            Cancel
          </Button>
          <Button
            variant={variant}
            onClick={onConfirm}
            disabled={loading}
            data-testid={testId ? `${testId}-confirm` : undefined}
          >
            {loading ? loadingLabel : confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
