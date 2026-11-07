// SPDX-License-Identifier: MIT
import { useState } from "react";
import { resolveFileSource } from "@/shared/lib/file-source";

import { cn } from "@/lib/utils";
import {
  isShowingDiff,
  useViewerStore,
  type CommitDiff,
  type TabDiff,
} from "@/shared/stores/viewer-store";

import { useContextGit } from "../store/use-context-git-store";
import { CHANGE_KIND_WORD, type NavChange } from "../lib/nav-changes";
import { commitWhen } from "../lib/history-graph";

/** SW-VE-16 Clean minimal / IDE-native: the segmented File/Changes toggle. 2 px inner-track
 *  padding gives the active segment the "lifted pill" feel without an outline.
 *
 *  **R5 token-palette honesty** — hover and active bg both resolve to `--ibx-lifted`, the
 *  dedicated "elevation step above `--ibx-muted`" dual-theme token minted for SW-VE-16.
 *  Why: an earlier attempt used `--ibx-card` for active and `--ibx-raise` for hover, but
 *  `--ibx-card` sinks BELOW `--ibx-muted` in dark (reading as a hole, not a lift) and
 *  `--ibx-raise` COLLIDES with `--ibx-muted` in light (zero visible delta). `--ibx-lifted`
 *  is defined above `--ibx-muted` in both themes, closing both symptoms with one token. */
const SEG_BTN =
  "inline-flex h-[22px] cursor-pointer items-center rounded px-2.5 text-[12px] font-medium text-muted-foreground transition-colors hover:bg-[var(--ibx-lifted)] hover:text-foreground aria-pressed:bg-[var(--ibx-lifted)] aria-pressed:text-foreground aria-pressed:shadow-[0_1px_2px_var(--ibx-shadow)] aria-pressed:hover:bg-[var(--ibx-lifted)] focus-visible:outline-none focus-visible:ring-[1.5px] focus-visible:ring-ring";

/** SW-VE-16: provider id → display label for the toolbar's `vs <forge> <origin/branch>`.
 *  Matches the title-case casing users recognize. **Returns `null` for an unmapped id** (future
 *  forge added Rust-side without the FE map update, or any id this FE doesn't recognize) — the
 *  caller then collapses to the plain `vs origin/<branch>` fallback instead of surfacing a
 *  lowercase provider leak like `vs gitea origin/main` (N7 drift-safety). The ref is NOT
 *  parenthesized — the mono weight + muted color is enough separation (R3 Clean minimal). */
const PROVIDER_LABEL: Record<string, string> = {
  github: "GitHub",
  gitlab: "GitLab",
  bitbucket: "Bitbucket",
};

function providerLabel(id: string | null | undefined): string | null {
  if (!id) return null;
  return PROVIDER_LABEL[id] ?? null;
}

/** SW-VE-16: readable diff summary — always carries the `lines` unit, honestly says `0 changes`
 *  for a binary/rename-only case, and only shows a sign that has a count behind it. */
export function formatLineSummary(added: number | undefined, removed: number | undefined): string {
  const a = added ?? 0;
  const r = removed ?? 0;
  if (a === 0 && r === 0) return "0 changes";
  const parts: string[] = [];
  if (a > 0) parts.push(`+${a}`);
  if (r > 0) parts.push(`−${r}`);
  return `${parts.join(" ")} lines`;
}

/** The signed `+N −M lines` numerics for a diff summary. Each half gets its own color (green
 *  add / red del) with tabular numerics so counts stack vertically; the unit word stays muted
 *  so the numerics carry the weight. Explicit whitespace between parts so textContent reads
 *  `+87 −12 lines`, not `+87−12lines`. */
function DiffNumerics({ added, removed }: { added: number; removed: number }) {
  return (
    <>
      {added > 0 && (
        <span
          className="font-mono tabular-nums font-medium"
          style={{ color: "var(--cs-status-added)" }}
        >
          +{added}
        </span>
      )}
      {added > 0 && removed > 0 && " "}
      {removed > 0 && (
        <span
          className="font-mono tabular-nums font-medium"
          style={{ color: "var(--cs-status-deleted)" }}
        >
          −{removed}
        </span>
      )}{" "}
      <span className="text-muted-foreground">lines</span>
    </>
  );
}

/** What a commit tab compares: "#23 · Migrate dashboards · maya · 5h · +12 −3 lines". A
 *  History-selected binary/rename-only commit has `0 changes`; preserve the pre-SW-VE-16
 *  behavior of hiding the trailing segment in that case rather than reading `· 0 changes` next
 *  to the author (M1 — CommitLine was never in scope for the "+87 → +87 −12 lines" refresh;
 *  only the vs-remote summary beside the File/Changes switch was). */
function CommitLine({ commit }: { commit: CommitDiff }) {
  const [now] = useState(() => Date.now());
  const summary = formatLineSummary(commit.added, commit.removed);
  return (
    <span className="min-w-0 truncate" data-testid="diff-side-commit">
      <b className="font-[550] text-foreground">
        {commit.pr ? `#${commit.pr} · ` : ""}
        {commit.subject}
      </b>
      {" · "}
      {commit.who} · {commitWhen(commit.committedAt, now)}
      {summary !== "0 changes" && <span className="font-mono tabular-nums"> · {summary}</span>}
    </span>
  );
}

/**
 * SW-VE-16: the left zone of the file-viewer toolbar — the `File | Changes` segmented switch
 * over a changed file's tab plus a readable `vs <forge> origin/<branch>` remote label and
 * a diff summary pill. Clean minimal / IDE-native direction (Mockup #2): sans body, pill
 * segmented control over a quiet track, mono reserved for the `origin/<branch>` ref + the
 * diff numerics (R3 carry-forward from #3). The parent `FileViewerToolbar` owns the band
 * chrome; this zone sits on the same baseline as the right-zone chips (AC-7).
 *
 * **R3 fix 1 — diff summary is a bordered pill at rest**, not a transparent hover-only tap
 * target. Resting pill carries the hierarchy of a headline element; hover amps it rather
 * than inventing it. Click → opens the Changes view, same seam as pressing the `Changes`
 * segment (AC-3/AC-10). `0 changes` (binary / rename-only) renders inert — AC-10 only wires
 * a button when there's a real diff to open (N1).
 *
 * Provider-aware label (AC-2): the Rust `context_repo_status_impl` resolves the `origin`
 * remote's forge via `provider_for_host` and surfaces it as `remoteProvider`. We render the
 * title-case label (`GitHub`/`GitLab`/`Bitbucket`); the base ref comes from `status.upstream`
 * (already dynamic — a stacked `feat/foo` branch shows `origin/feat/foo`). No provider or no
 * remote → `vs origin/<branch>` or `vs origin` fallback.
 */
export function DiffSideStrip({
  teamKey,
  filePath,
  change,
  diff,
}: {
  teamKey: string;
  filePath: string;
  change: NavChange | undefined;
  diff: TabDiff | null;
}) {
  const showingChanges = isShowingDiff(diff);
  const commit = diff && diff !== true ? diff : null;
  const status = useContextGit(teamKey).status;
  const sourceKind = useViewerStore((s) => s.navLens[teamKey]?.source?.kind ?? "clone");
  const localOnly = resolveFileSource(filePath).source.kind === "private";
  const setTabDiff = useViewerStore((s) => s.setTabDiff);
  if ((localOnly ? "private" : "clone") !== sourceKind) return null;
  // N2: a tracking branch with a null `upstream` (hasUpstream=true but the short name went
  // unresolved on the Rust side — the TS types permit it independently) falls back to a
  // synthesized `origin/<branch>` rather than the bare `origin`, so the toolbar still names a
  // ref the user can relate to. Only full absence (no tracking at all) collapses to `origin`.
  const against = localOnly
    ? "HEAD · local only"
    : (status?.upstream ??
      (status?.hasUpstream && status.branch ? `origin/${status.branch}` : "origin"));
  const forge = localOnly ? null : providerLabel(status?.remoteProvider);
  const summary = change ? formatLineSummary(change.added, change.removed) : "";
  // N1: `0 changes` (binary / rename-only) is NOT an actionable affordance — clicking would
  // open an empty Changes view with no diff to read. Render it as inert text; only emit the
  // `<button>` seam when there's a real diff behind the summary.
  const summaryIsActionable = summary !== "" && summary !== "0 changes";
  const openChanges = () =>
    setTabDiff(teamKey, filePath, commit ? { ...commit, showFile: false } : true);

  return (
    <div
      className="flex min-w-0 flex-1 items-center gap-2.5 text-[13px] text-muted-foreground"
      data-testid="diff-side-strip"
    >
      {/* Segmented File/Changes — 22 px pill inside a quiet 2 px-padded track. The track sits
          BELOW the chip-rest surface so the pressed chip (`--ibx-card`) lifts above it; using
          `--ibx-muted` for the track puts us on the same semantic level as the toolbar band's
          own background. R4 token hygiene — no parallel color-mix recipes. */}
      <div
        role="group"
        aria-label="Show"
        className="inline-flex shrink-0 items-center rounded-md p-0.5"
        style={{ backgroundColor: "var(--ibx-muted)" }}
      >
        <button
          type="button"
          aria-pressed={!showingChanges}
          className={SEG_BTN}
          data-testid="diff-side-file"
          disabled={!commit && change?.kind === "deleted"}
          onClick={() =>
            setTabDiff(teamKey, filePath, commit ? { ...commit, showFile: true } : false)
          }
        >
          File
        </button>
        <button
          type="button"
          aria-pressed={showingChanges}
          className={SEG_BTN}
          data-testid="diff-side-changes"
          onClick={openChanges}
        >
          Changes
        </button>
      </div>

      <div className="file-viewer-summary">
        {commit && showingChanges ? (
          <CommitLine commit={commit} />
        ) : (
          <div className="flex min-w-0 items-center gap-2">
            <span className="truncate text-[13px]">
              <span className="text-muted-foreground">vs </span>
              {forge && <span className="font-medium text-foreground">{forge} </span>}
              <span
                className="font-mono text-[12px]"
                style={{ color: forge ? "var(--muted-foreground)" : "var(--foreground)" }}
              >
                {against}
              </span>
            </span>
            {change &&
              (summaryIsActionable ? (
                <button
                  type="button"
                  data-testid="diff-side-summary"
                  aria-label={`Open changes: ${summary}`}
                  onClick={openChanges}
                  className={cn(
                    "inline-flex h-[22px] cursor-pointer items-center gap-1.5 rounded border px-2 text-[12px] transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-[1.5px] focus-visible:ring-ring",
                  )}
                  style={{
                    borderColor: "var(--ibx-border, var(--border))",
                    backgroundColor: "var(--ibx-card, var(--card))",
                  }}
                >
                  <DiffNumerics added={change.added ?? 0} removed={change.removed ?? 0} />
                </button>
              ) : (
                <span
                  data-testid="diff-side-summary"
                  className="inline-flex h-[22px] items-center rounded border px-2 text-[12px] text-muted-foreground"
                  style={{
                    borderColor: "var(--ibx-border, var(--border))",
                    backgroundColor: "var(--ibx-card, var(--card))",
                  }}
                >
                  {summary || CHANGE_KIND_WORD[change.kind].toLowerCase()}
                </span>
              ))}
          </div>
        )}
      </div>
    </div>
  );
}
