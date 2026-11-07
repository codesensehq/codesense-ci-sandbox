// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { AlertTriangle, Loader2, Plus, X } from "lucide-react";
import { AppButton } from "@/shared/components/app-button";

export interface EpicRepoCardProps {
  /** The epic's dir leaf (`epicDirPath`) — the stable key the owner resolves to
   *  the epic doc path. The card never touches the doc path itself. */
  epicDirPath: string;
  /** Team's attached source repos as `owner/repo` — the add-picker source. */
  teamSourceRepos: readonly string[];
  /** Slugs that have a local clone; others render a "needs clone" marker
   *  (config-store `localPath` nullity — the single clone predicate). */
  clonedRepos: ReadonlySet<string>;
  /** Read the epic's `repos:` from the doc (source of truth). */
  onReadEpicRepos: (epicDirPath: string) => Promise<string[]>;
  /** Persist the full desired `repos:` list and resolve with the re-read list
   *  (source of truth, one round-trip). `assemble: true` (an add) fires worktree
   *  assembly fire-and-forget on the owner side — this promise never awaits it,
   *  so the UI is non-blocking. `assemble: false` (a remove) writes only. */
  onSetEpicRepos: (
    epicDirPath: string,
    repos: string[],
    opts: { assemble: boolean },
  ) => Promise<string[]>;
}

/**
 * SW-EW-9 / SLW-FX-15 — the epic's configured `repos:` as page content in the
 * "Configure repos" overlay (`EpicRepoOverlay`, its sole consumer): per-repo
 * remove (×) and an "+ add repo" picker drawn from the team's attached source
 * repos minus what's already on the epic. SLW-FX-15 page-scaled the card
 * (raised `.cs-workspace-card` surface, token colors, promoted add CTA).
 *
 * Source of truth is the epic doc: the card reads on mount and re-renders from
 * the list every write returns — it never holds the canonical set in app state.
 * Add is non-blocking: `onSetEpicRepos` resolves as soon as the doc write lands;
 * worktree assembly runs fire-and-forget on the owner side.
 *
 * a11y: a `role="group"` of `role="treeitem"` rows; native buttons (Enter/Space)
 * for remove/add; focus-visible rings; `--cs-*` / token-driven colors for
 * multi-theme parity.
 */
export function EpicRepoCard({
  epicDirPath,
  teamSourceRepos,
  clonedRepos,
  onReadEpicRepos,
  onSetEpicRepos,
}: EpicRepoCardProps) {
  const [repos, setRepos] = useState<string[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setError(null);
    onReadEpicRepos(epicDirPath)
      .then((list) => {
        if (!cancelled) setRepos(list);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [epicDirPath, onReadEpicRepos]);

  const current = repos ?? [];
  const addable = teamSourceRepos.filter((slug) => !current.includes(slug));

  const runMutation = async (op: () => Promise<string[]>) => {
    setBusy(true);
    setError(null);
    try {
      const next = await op();
      setRepos(next);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleAdd = (slug: string) => {
    setPickerOpen(false);
    void runMutation(() => onSetEpicRepos(epicDirPath, [...current, slug], { assemble: true }));
  };
  const handleRemove = (slug: string) =>
    void runMutation(() =>
      onSetEpicRepos(
        epicDirPath,
        current.filter((s) => s !== slug),
        { assemble: false },
      ),
    );

  return (
    <div
      role="group"
      aria-label={`Repos for ${epicDirPath}`}
      data-testid={`epic-repo-card-${epicDirPath}`}
      // SLW-FX-15: page-content surface — no tree-indent (left-aligns with the
      // overlay header), page `text-sm` scale, and the SW-EW-12 raised idiom
      // (`.cs-workspace-card`) instead of ad-hoc `bg-card/40 border-border/60`.
      className="flex flex-col gap-1 rounded-md p-2 text-sm cs-workspace-card"
      onKeyDown={(e) => {
        // Esc collapses the picker first; otherwise let the panel handle it.
        if (e.key === "Escape" && pickerOpen) {
          e.stopPropagation();
          setPickerOpen(false);
        }
      }}
    >
      {repos === null && error === null ? (
        <div className="flex items-center gap-1.5 px-1 py-1 text-muted-foreground">
          <Loader2 className="animate-spin" style={{ width: "1em", height: "1em" }} />
          <span>Loading repos…</span>
        </div>
      ) : (
        <>
          {current.length === 0 && (
            // SLW-FX-15: empty state invites the add action (the promoted CTA
            // renders directly below), rather than reading as a dead line.
            <div className="px-1 py-1 text-sm text-muted-foreground">
              No repos yet — add the source repos this epic assembles.
            </div>
          )}
          {current.map((slug) => {
            const needsClone = !clonedRepos.has(slug);
            return (
              <div
                key={slug}
                role="treeitem"
                aria-label={slug}
                className="group/repo flex items-center gap-1.5 rounded-md px-2 py-1.5 cs-sidebar-item"
              >
                <span className="truncate">{slug}</span>
                {needsClone && (
                  <span
                    // SLW-FX-15: token warning hue (`--cs-status-modified`, the
                    // theme-aware amber/gold pair) instead of raw `amber-*`.
                    className="flex-shrink-0 inline-flex items-center gap-0.5 text-xs text-[color:var(--cs-status-modified)]"
                    data-testid={`epic-repo-needs-clone-${slug}`}
                  >
                    <AlertTriangle aria-hidden="true" style={{ width: "0.9em", height: "0.9em" }} />
                    <span>needs clone</span>
                  </span>
                )}
                <button
                  type="button"
                  aria-label={`Remove ${slug}`}
                  disabled={busy}
                  onClick={() => handleRemove(slug)}
                  // SLW-FX-15: discoverable at rest (`opacity-60`) — a config
                  // surface's primary destructive control shouldn't be fully
                  // hover-hidden — lifting to full opacity on hover/focus.
                  className="ml-auto flex-shrink-0 opacity-60 group-hover/repo:opacity-100 focus-visible:opacity-100 transition-opacity disabled:opacity-40 focus-visible:outline focus-visible:outline-1 focus-visible:outline-accent rounded"
                >
                  <X style={{ width: "1em", height: "1em" }} />
                </button>
              </div>
            );
          })}

          {pickerOpen ? (
            // SLW-FX-15: the transient add-menu rows share the page rhythm
            // (`text-sm`, `px-2 py-1.5`) so they don't read as a half-styled
            // leftover next to the promoted CTA — behavior/testids unchanged.
            <div role="group" aria-label="Add a repo" className="flex flex-col gap-1">
              {addable.length === 0 ? (
                <div className="px-1 py-1 text-sm text-muted-foreground">
                  {teamSourceRepos.length === 0
                    ? "No team source repos attached."
                    : "All team repos are already on this epic."}
                </div>
              ) : (
                addable.map((slug) => (
                  <button
                    key={slug}
                    type="button"
                    disabled={busy}
                    onClick={() => handleAdd(slug)}
                    className="flex items-center gap-1.5 rounded-md px-2 py-1.5 text-left cs-sidebar-item disabled:opacity-40 focus-visible:outline focus-visible:outline-1 focus-visible:outline-accent"
                  >
                    <Plus className="flex-shrink-0" style={{ width: "1em", height: "1em" }} />
                    <span className="truncate">{slug}</span>
                  </button>
                ))
              )}
            </div>
          ) : (
            // SLW-FX-15: the add CTA is the panel's primary action — a real
            // shared `AppButton` (shadcn `Button`, `data-slot="button"`), not
            // the faintest ghost row. Testid + wiring ride through unchanged.
            <AppButton
              type="button"
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={() => setPickerOpen(true)}
              data-testid={`epic-repo-add-${epicDirPath}`}
              className="self-start"
            >
              {busy ? <Loader2 className="animate-spin" /> : <Plus />}
              <span>add repo</span>
            </AppButton>
          )}

          {error !== null && (
            // SLW-FX-15: token alert container (shadcn `destructive` idiom) so
            // an error reads as an alert, not a stray red line.
            <div
              className="flex items-center gap-1.5 rounded-md border border-destructive/40 bg-destructive/10 px-2 py-1.5 text-destructive"
              role="alert"
              data-testid={`epic-repo-error-${epicDirPath}`}
            >
              <AlertTriangle
                aria-hidden="true"
                className="flex-shrink-0"
                style={{ width: "1em", height: "1em" }}
              />
              <span>{error}</span>
            </div>
          )}
        </>
      )}
    </div>
  );
}
