// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";

import { AppButton } from "@/shared/components/app-button";
import { cn } from "@/lib/utils";

/** One action an empty state offers. */
export interface EmptyStateAction {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  testId?: string;
}

/**
 * The one empty state (consistency spec, Rule 3): an optional icon or sketch, a
 * short title, one sentence, and at most ONE primary action — plus an optional
 * secondary link. It never renders nothing: the title is required. `page` is the
 * centered floor of a whole content area (first run, boot problems); `panel` is a
 * state inside a panel, tab section or list.
 *
 * SW-FU-C1 merged the three variants that grew separately (the first-run
 * shell, the coached first-run state and the gate drawer's small one) into this.
 */
export function EmptyState({
  icon,
  title,
  body,
  action,
  secondaryAction,
  size = "panel",
  testId,
  className,
  children,
}: {
  /** An icon element or a decorative {@link EmptySketch}. */
  icon?: ReactNode;
  title: string;
  /** One sentence; rich content (e.g. a list) is allowed where the state needs it. */
  body?: ReactNode;
  action?: EmptyStateAction;
  /** A quiet link, or a short row of them (a first screen allows at most two). */
  secondaryAction?: EmptyStateAction | readonly EmptyStateAction[];
  size?: "page" | "panel";
  testId?: string;
  className?: string;
  /** Extra content under the actions (e.g. evidence under a gate state). */
  children?: ReactNode;
}): React.JSX.Element {
  const page = size === "page";
  const secondaries: readonly EmptyStateAction[] = !secondaryAction
    ? []
    : Array.isArray(secondaryAction)
      ? secondaryAction
      : [secondaryAction as EmptyStateAction];
  return (
    <div
      data-slot="empty-state"
      data-testid={testId}
      className={cn(
        "flex flex-col items-center gap-2 text-center",
        page ? "px-6 py-12" : "px-3 py-8",
        className,
      )}
    >
      {icon && (
        <div
          className={cn(
            "mb-1 flex justify-center text-muted-foreground",
            page ? "[&>svg:not([class*='size-'])]:size-8" : "[&>svg:not([class*='size-'])]:size-6",
          )}
          aria-hidden
        >
          {icon}
        </div>
      )}
      {page ? (
        <h2 className="text-base font-medium text-foreground">{title}</h2>
      ) : (
        <p className="text-sm font-medium text-foreground">{title}</p>
      )}
      {body && <div className="max-w-sm text-xs text-muted-foreground">{body}</div>}
      {action && (
        <AppButton
          type="button"
          size="sm"
          className="mt-2"
          onClick={action.onClick}
          disabled={action.disabled}
          data-testid={action.testId}
        >
          {action.label}
        </AppButton>
      )}
      {secondaries.length > 0 && (
        <div className="flex flex-wrap items-center justify-center gap-x-3">
          {secondaries.map((secondary) => (
            <AppButton
              key={secondary.label}
              type="button"
              variant="link"
              size="xs"
              className="text-muted-foreground"
              onClick={secondary.onClick}
              disabled={secondary.disabled}
              data-testid={secondary.testId}
            >
              {secondary.label}
            </AppButton>
          ))}
        </div>
      )}
      {children}
    </div>
  );
}

/** Faint sketches that reinforce what an empty surface will hold. Decorative. */
export function EmptySketch({ kind }: { kind: "lanes" | "pipeline" | "tile" | "tree" }) {
  const bar = "rounded bg-muted-foreground/15";
  if (kind === "lanes") {
    return (
      <div aria-hidden className="flex w-32 flex-col gap-1.5">
        {[0, 1, 2].map((i) => (
          <div key={i} className="flex items-center gap-1.5">
            <span className="size-2 rounded-full bg-muted-foreground/25" />
            <span className={`h-1.5 flex-1 ${bar}`} style={{ marginRight: `${i * 14}px` }} />
          </div>
        ))}
      </div>
    );
  }
  if (kind === "pipeline") {
    return (
      <div aria-hidden className="flex items-center gap-1 opacity-60">
        {[0, 1, 2].map((i) => (
          <div key={i} className="flex items-center gap-1">
            {i > 0 && <span className="h-px w-3 bg-muted-foreground/30" />}
            <span className="h-5 w-9 rounded border border-muted-foreground/25 bg-muted/40" />
          </div>
        ))}
      </div>
    );
  }
  if (kind === "tile") {
    return (
      <div
        aria-hidden
        className="flex h-10 w-24 flex-col justify-end gap-1 rounded border border-dashed border-muted-foreground/30 p-1.5"
      >
        <span className={`h-1.5 w-2/3 ${bar}`} />
        <span className={`h-1.5 w-1/3 ${bar}`} />
      </div>
    );
  }
  return (
    <div aria-hidden className="flex w-28 flex-col gap-1">
      <span className={`h-1.5 w-3/4 ${bar}`} />
      <span className={`ml-3 h-1.5 w-2/3 ${bar}`} />
      <span className={`ml-3 h-1.5 w-1/2 ${bar}`} />
    </div>
  );
}
