// SPDX-License-Identifier: MIT
import { FileWarning, type LucideIcon } from "lucide-react";

/**
 * Centered "can't render this file inline" placeholder — the WD-2 binary
 * affordance idiom (FileWarning icon + a message + the filename). Shared by the
 * source-diff pane (binary file) and the read-only editor view (binary / too
 * large) so the two surfaces read identically. No "open" action by design.
 */
export function FileWarningPlaceholder({
  message,
  filename,
  testId,
  icon: Icon = FileWarning,
}: {
  message: string;
  filename: string;
  testId?: string;
  /** Defaults to the warning glyph; a plain-fact state (a binary file in a
   *  diff) passes a neutral one. */
  icon?: LucideIcon;
}) {
  return (
    <div
      className="flex h-full flex-col items-center justify-center gap-3 px-8 text-center"
      data-testid={testId}
    >
      <Icon className="size-8 text-muted-foreground" aria-hidden />
      <div className="space-y-1">
        <p className="text-sm font-medium">{message}</p>
        <p className="font-mono text-xs text-muted-foreground">{filename}</p>
      </div>
    </div>
  );
}
