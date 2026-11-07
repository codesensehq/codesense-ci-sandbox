// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";

interface SettingsCardProps {
  /** Card header text — rendered as a bold label */
  title: string;
  /** Optional count badge, e.g. "(4)" appended to title */
  count?: number;
  /** Optional slot for header-level actions (e.g. "+ Add" button) */
  headerAction?: ReactNode;
  children: ReactNode;
  /** data-testid for the card wrapper */
  "data-testid"?: string;
}

export function SettingsCard({
  title,
  count,
  headerAction,
  children,
  "data-testid": testId,
}: SettingsCardProps) {
  return (
    <div className="mb-4 rounded-lg border border-border p-4" data-testid={testId}>
      <div className="mb-3 flex items-center justify-between">
        <h3 className="text-sm font-semibold text-foreground">
          {title}
          {count != null && <span className="ml-1 text-muted-foreground">({count})</span>}
        </h3>
        {headerAction}
      </div>
      {children}
    </div>
  );
}
