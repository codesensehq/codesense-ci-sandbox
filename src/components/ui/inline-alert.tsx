// SPDX-License-Identifier: MIT
import * as React from "react";
import { AlertTriangle } from "lucide-react";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";
import { errorReasonLine } from "@/shared/lib/error-message";

import { Button } from "./button";

/**
 * SW-MC-6 (AC-10): the shared inline-callout primitive. Before this, the same
 * `AlertTriangle + text` block was hand-rolled at ~7 call sites across two
 * severities (amber warning / destructive error). Extracting it here keeps the
 * severity tokens in one place and makes every call site a one-liner.
 *
 * `severity` picks the token set; the base layout is shared. Minor visual
 * normalization vs. the pre-extraction hand-rolls (unified padding / gap /
 * radius / icon size) is intentional — the point is a single consistent
 * callout, not per-site pixel drift.
 *
 * SW-FU-C1: it is also the one inline error (consistency spec, Rule 5). An error
 * says what failed (`title`), why in words (`error`, read through
 * `userFacingErrorMessage` so a raw thrown value never reaches the screen, or the
 * children), and one next action (`action`).
 */
const inlineAlertVariants = cva("flex items-start gap-2 rounded-md px-3 py-2 text-xs", {
  variants: {
    severity: {
      warning: "bg-amber-500/10 text-(--cs-warning-text)",
      error: "border border-destructive/30 bg-destructive/10 text-destructive",
    },
  },
  defaultVariants: {
    severity: "warning",
  },
});

/** The one next action an alert offers, rendered as the shared outline button. */
export interface InlineAlertAction {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  testId?: string;
  autoFocus?: boolean;
}

export interface InlineAlertProps
  extends Omit<React.ComponentProps<"div">, "title">, VariantProps<typeof inlineAlertVariants> {
  /** Hide the leading icon (shown by default). */
  hideIcon?: boolean;
  /** What failed, in a few words ("Couldn't remove acme/web"). */
  title?: React.ReactNode;
  /** The thrown value behind the failure. Shown as its user-facing reason, never raw. */
  error?: unknown;
  /** Optional trailing action (e.g. a "Re-check" / "Go to it" button). When
   *  present, the message flexes to fill and the action sits on the right. */
  action?: React.ReactNode | InlineAlertAction;
}

/** The reason in words for a thrown value, or nothing when it carries none (a
 *  missing value, an empty message, or an object with no message of its own). */
function errorReason(error: unknown): string | null {
  return errorReasonLine(error);
}

function isActionSpec(action: InlineAlertProps["action"]): action is InlineAlertAction {
  return (
    typeof action === "object" &&
    action !== null &&
    !React.isValidElement(action) &&
    "label" in action &&
    "onClick" in action
  );
}

export function InlineAlert({
  severity = "warning",
  className,
  children,
  hideIcon = false,
  title,
  error,
  action,
  ...props
}: InlineAlertProps) {
  const actionNode = isActionSpec(action) ? (
    <Button
      type="button"
      size="sm"
      variant="outline"
      className={severity === "warning" ? "shrink-0 text-foreground" : "shrink-0"}
      onClick={action.onClick}
      disabled={action.disabled}
      autoFocus={action.autoFocus}
      data-testid={action.testId}
    >
      {action.label}
    </Button>
  ) : (
    action
  );
  const reason = errorReason(error);
  const message = (
    <div className="min-w-0">
      {title != null && title !== false && title !== "" && (
        <div data-slot="inline-alert-title" className="font-medium">
          {title}
        </div>
      )}
      {reason && <div>{reason}</div>}
      {children}
    </div>
  );
  return (
    <div
      role="alert"
      data-slot="inline-alert"
      data-severity={severity ?? "warning"}
      className={cn(inlineAlertVariants({ severity }), className)}
      {...props}
    >
      {!hideIcon && <AlertTriangle size={14} className="mt-0.5 shrink-0" aria-hidden="true" />}
      {actionNode ? (
        <div className="flex flex-1 items-center justify-between gap-2">
          {message}
          {actionNode}
        </div>
      ) : (
        message
      )}
    </div>
  );
}
