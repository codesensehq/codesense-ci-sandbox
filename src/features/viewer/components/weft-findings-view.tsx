// SPDX-License-Identifier: MIT
import { ChevronRight } from "lucide-react";

import type { PerNodeResult, StepArtifact } from "@/shared/lib/run-evidence";

/**
 * SW-WV-F4 (AC-3, AC-4) — the Findings tab body: the operator's altitude view of a
 * worker run. One row per reported node with a non-empty `summary`; the diff is always
 * one click away (never hidden) via a "View diff" chevron that opens the finding's file
 * through the existing viewer seam (`onOpenDiff` → `openFile`). No second diff surface.
 *
 * AC-4's altitude-integrity badge is informational only — it flags a summary that
 * name-drops a token (a word ≥ 4 chars or a `src/`/`lib/`/`./` path) NOT found in the
 * node's produced artifacts, nudging the reviewer to verify against the diff before
 * ratifying. It never blocks ratification and never false-alarms when the node produced
 * no file artifacts.
 */
export interface WeftFindingsViewProps {
  perNodeResults: PerNodeResult[];
  /** Open the finding's file in the viewer (the diff overlay activates there). */
  onOpenDiff: (path: string) => void;
}

/** The first `file`-kind artifact on a node, or `null` — the chevron's diff target.
 *  Absent → no chevron (no dead button, AD-10). */
function firstFileArtifact(result: PerNodeResult): StepArtifact | null {
  return result.artifacts?.find((a) => a.kind === "file") ?? null;
}

/** Strip surrounding punctuation from a whitespace-split token. */
function cleanToken(raw: string): string {
  return raw.replace(/[.,;:!?)"'\]]+$/, "").replace(/^[("'[]+/, "");
}

/**
 * A token warrants a diff check only when it is clearly a FILE PATH: it contains a `/`
 * (a path), or it ends in a real file extension (`.ts`, `.tsx`, `.rs`, … — an alpha
 * extension of 1-4 chars). This deliberately excludes prose that merely contains a dot
 * — version strings (`1.2.3`), abbreviations (`e.g.`), and sentence punctuation don't
 * qualify, so they never raise a false "verify against diff" alarm.
 */
function isFilePathToken(t: string): boolean {
  if (t.length < 4) return false;
  if (t.startsWith("src/") || t.startsWith("lib/") || t.startsWith("./")) return true;
  if (t.includes("/")) return true;
  return /\.[a-zA-Z]{1,4}$/.test(t);
}

/**
 * A summary path token is "covered" by the artifacts when some artifact reference names
 * the same file — an exact match OR the reference ends with `/<token>` (the token is the
 * artifact's basename or a suffix path segment). Segment-aware, so `index.ts` is NOT
 * considered covered by an unrelated `src/parser/index.ts` unless the token is that
 * file's actual tail — avoiding the substring false-negative where any shared basename
 * silently suppressed a legitimate mismatch.
 */
function tokenCoveredByArtifacts(token: string, references: string[]): boolean {
  return references.some((ref) => ref === token || ref.endsWith(`/${token}`));
}

/**
 * AC-4: does the summary name a file-path token absent from the node's artifact
 * references? Informational only (never blocks ratification). Suppressed entirely when
 * the node has no artifacts (never a false alarm when the worker produced no file
 * artifacts).
 */
function hasAltitudeMismatch(result: PerNodeResult): boolean {
  const references = (result.artifacts ?? []).map((a) => a.reference);
  if (references.length === 0) return false;
  const tokens = (result.summary ?? "").split(/\s+/).map(cleanToken).filter(isFilePathToken);
  return tokens.some((t) => !tokenCoveredByArtifacts(t, references));
}

/** AC-4: the "verify against diff" chip — informational, does not block ratification. */
function MismatchBadge(): React.JSX.Element {
  return (
    <span
      data-testid="findings-mismatch-badge"
      title="A term in this summary is not in the produced diff — verify before ratifying."
      className="shrink-0 rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] font-medium text-amber-600"
    >
      verify against diff
    </span>
  );
}

export function WeftFindingsView({
  perNodeResults,
  onOpenDiff,
}: WeftFindingsViewProps): React.JSX.Element {
  const rows = perNodeResults.filter((r) => (r.summary ?? "").trim().length > 0);

  if (rows.length === 0) {
    return (
      <p data-testid="findings-empty" className="px-3 py-4 text-sm text-muted-foreground">
        No findings recorded
      </p>
    );
  }

  return (
    <ul className="flex flex-col divide-y divide-border">
      {rows.map((result) => {
        const fileArtifact = firstFileArtifact(result);
        const mismatch = hasAltitudeMismatch(result);
        return (
          <li
            key={result.nodeId}
            data-testid="findings-row"
            className="flex items-center gap-2 px-3 py-2 text-sm"
          >
            <span className="flex-1">{result.summary}</span>
            {mismatch && <MismatchBadge />}
            {fileArtifact && (
              <button
                type="button"
                data-testid="findings-diff-chevron"
                onClick={() => onOpenDiff(fileArtifact.reference)}
                title={`View diff — ${fileArtifact.reference}`}
                className="inline-flex shrink-0 items-center gap-0.5 rounded px-1.5 py-0.5 text-xs text-muted-foreground transition hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                View diff
                <ChevronRight size={14} aria-hidden />
              </button>
            )}
          </li>
        );
      })}
    </ul>
  );
}
