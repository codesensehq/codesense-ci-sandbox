// SPDX-License-Identifier: MIT
import { AlertTriangle, X } from "lucide-react";
import type { DocProblem } from "@/features/dashboard/components/html-viewer";
/** The most recent problems a dev banner lists; older ones are counted. */
const SHOWN_PROBLEMS = 3;
export function ProblemBanner({
  problems,
  total,
  onDismiss,
}: {
  problems: DocProblem[];
  total: number;
  /** Absent when the problem lasts until the next listing replaces it. */
  onDismiss?: () => void;
}) {
  const shown = problems.slice(-SHOWN_PROBLEMS);
  const hidden = total - shown.length;
  return (
    <div
      role="alert"
      data-testid="pane-dev-banner"
      className="flex flex-shrink-0 items-start gap-2 border-b border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-foreground"
    >
      <AlertTriangle size={14} className="mt-0.5 flex-shrink-0 text-destructive" />
      <div className="min-w-0 flex-1 space-y-0.5">
        {hidden > 0 && <p className="text-muted-foreground">{hidden} earlier</p>}
        {shown.map((p, i) => (
          <p key={`${i}-${p.message}`} className="break-words font-mono">
            <span className="font-sans font-medium">
              {p.kind === "load" ? "Couldn't load" : p.kind === "bridge" ? "Call failed" : "Error"}:
            </span>{" "}
            {p.message}
          </p>
        ))}
      </div>
      {onDismiss && (
        <button
          type="button"
          onClick={onDismiss}
          aria-label="Dismiss pane errors"
          className="flex-shrink-0 rounded p-0.5 text-muted-foreground hover:bg-destructive/15 hover:text-foreground"
        >
          <X size={14} />
        </button>
      )}
    </div>
  );
}
