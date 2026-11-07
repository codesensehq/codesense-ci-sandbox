// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

/**
 * The layout every management tab shares (consistency spec, Rules 1 and 2): a
 * scrollable surface on the app theme, a centered column, a header whose ONE
 * primary action sits top-right, and titled sections.
 *
 * `text-foreground` matters: a virtual tab renders inside the team pane, which
 * inherits the terminal theme's color, so the surface re-establishes the app's.
 */
export function TabSurface({
  testId,
  className,
  children,
}: {
  testId?: string;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div
      data-slot="tab-surface"
      data-testid={testId}
      className={cn(
        "flex h-full min-h-0 flex-col overflow-y-auto bg-background text-foreground",
        className,
      )}
    >
      <div className="mx-auto flex w-full max-w-3xl flex-col gap-6 p-6">{children}</div>
    </div>
  );
}

/** A tab's title and one line on what it is for, with its actions top-right. */
export function TabHeader({
  title,
  description,
  actions,
  children,
}: {
  title: ReactNode;
  description?: ReactNode;
  /** The surface's primary action (and any outline secondaries to its left). */
  actions?: ReactNode;
  /** Extra lines under the description. */
  children?: ReactNode;
}) {
  return (
    <header className="flex items-start gap-4">
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <h2 className="text-lg font-semibold">{title}</h2>
        {description && <p className="text-sm text-muted-foreground">{description}</p>}
        {children}
      </div>
      {actions && (
        <div data-slot="tab-header-actions" className="flex shrink-0 items-center gap-2">
          {actions}
        </div>
      )}
    </header>
  );
}

/** A titled block inside a tab. `card` frames it (e.g. an add form). */
export function TabSection({
  title,
  description,
  actions,
  card = false,
  testId,
  children,
}: {
  title: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  card?: boolean;
  testId?: string;
  children: ReactNode;
}) {
  return (
    <section
      data-slot="tab-section"
      data-testid={testId}
      className={cn(
        "flex flex-col",
        card ? "gap-4 rounded-lg border border-border bg-card/40 p-5" : "gap-2",
      )}
    >
      <div className="flex items-start gap-2">
        <div className="flex min-w-0 flex-1 flex-col gap-1">
          <h3 className="text-sm font-semibold">{title}</h3>
          {description && <p className="text-xs text-muted-foreground">{description}</p>}
        </div>
        {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
      </div>
      {children}
    </section>
  );
}
