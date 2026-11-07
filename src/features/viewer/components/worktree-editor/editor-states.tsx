// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

/** Loading placeholder shaped like code — staggered line widths in the gutter
 *  and text column — so the switch to the real editor doesn't jump. */
export function EditorSkeleton() {
  const widths = [62, 48, 71, 35, 0, 56, 80, 44, 67, 30, 0, 52, 74, 39];
  return (
    <div
      aria-hidden
      data-testid="worktree-file-skeleton"
      className="flex h-full flex-col gap-[0.45rem] overflow-hidden px-4 py-3"
    >
      {widths.map((w, i) => (
        <div key={i} className="flex items-center gap-4">
          <span className="h-2.5 w-5 shrink-0 rounded-sm bg-muted/70" />
          {w > 0 && (
            <span
              className="h-2.5 animate-pulse rounded-sm bg-muted"
              style={{ width: `${w}%`, animationDelay: `${i * 40}ms` }}
            />
          )}
        </div>
      ))}
    </div>
  );
}

/** A centred message in the editor area: an error with Retry, a removed file,
 *  the empty workbench. */
export function EditorStateMessage({
  title,
  detail,
  action,
  tone = "muted",
  testId,
  children,
}: {
  title: ReactNode;
  detail?: ReactNode;
  action?: ReactNode;
  tone?: "muted" | "destructive";
  testId?: string;
  children?: ReactNode;
}) {
  return (
    <div
      role={tone === "destructive" ? "alert" : undefined}
      data-testid={testId}
      className="flex h-full flex-col items-center justify-center gap-2 px-8 text-center"
    >
      <p
        className={cn(
          "text-sm font-medium",
          tone === "destructive" ? "text-destructive" : "text-foreground",
        )}
      >
        {title}
      </p>
      {detail && (
        <p
          className="max-w-[48ch] truncate text-xs text-muted-foreground"
          title={typeof detail === "string" ? detail : undefined}
        >
          {detail}
        </p>
      )}
      {children}
      {action && <div className="pt-1">{action}</div>}
    </div>
  );
}

/** A keyboard key as the status bar and empty state show it. */
export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="inline-flex h-5 min-w-5 items-center justify-center rounded border border-border bg-background px-1 font-sans text-[0.68rem] font-medium text-muted-foreground shadow-[0_1px_0_var(--border)]">
      {children}
    </kbd>
  );
}
