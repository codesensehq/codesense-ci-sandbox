// SPDX-License-Identifier: MIT
import { AlertTriangle, CircleAlert, CircleCheck } from "lucide-react";
import type { StoryValidationReport } from "@/tauri/commands";

/**
 * The `validate_story` findings for one story, blocking first. Used by the
 * New-story form (live, before writing) and the metadata band (on-disk file),
 * so an author sees exactly what an agent calling `validate_story` would.
 */
export function StoryFindings({
  report,
  showWhenClean = false,
}: {
  report: StoryValidationReport;
  /** Render a "No problems found" line for a clean report. */
  showWhenClean?: boolean;
}) {
  const findings = report.findings;
  if (findings.length === 0) {
    return showWhenClean ? (
      <p
        data-testid="story-findings-clean"
        className="flex items-center gap-1.5 text-xs text-muted-foreground"
      >
        <CircleCheck className="size-3.5 text-emerald-600" aria-hidden />
        No problems found.
      </p>
    ) : null;
  }
  return (
    <ul
      data-testid="story-findings"
      aria-label="Story validation findings"
      className="flex flex-col gap-1.5"
    >
      {findings.map((f, i) => {
        const blocking = f.severity === "blocking";
        const Icon = blocking ? CircleAlert : AlertTriangle;
        return (
          <li
            key={`${f.code}-${i}`}
            data-code={f.code}
            data-severity={f.severity}
            className="flex items-start gap-1.5 text-xs"
          >
            <Icon
              className={`mt-0.5 size-3.5 shrink-0 ${blocking ? "text-destructive" : "text-amber-600"}`}
              aria-hidden
            />
            <span>
              <span className="font-medium">{blocking ? "Blocking" : "Advisory"}:</span> {f.message}
              {f.hint ? <span className="block text-muted-foreground">{f.hint}</span> : null}
            </span>
          </li>
        );
      })}
    </ul>
  );
}
