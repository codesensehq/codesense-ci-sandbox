// SPDX-License-Identifier: MIT
import type React from "react";
import { CornerLeftUp } from "lucide-react";

import { RepoGlyph } from "@/shared/components/repo-glyph";
import { getRepoColor } from "@/shared/lib/pipeline-graph/epic-colors";
import { type FileScope, scopeLabel } from "@/features/dashboard/lib/file-scope";

// ---------------------------------------------------------------------------
// SW-VE-5 — scope legibility badges + edit chrome. Presentational leaves over the
// pure `FileScope` model (`file-scope.ts`). Reuses the existing provenance
// vocabulary (`RepoGlyph` + `getRepoColor`) — NO new color system. This module is
// the provenance-badge home: `NavProvenance` + `SourceRepoTint` moved here (from
// `pipeline-status-nav.tsx`) so the nav and the scope badges share one source and
// there is no import cycle (this leaf imports nothing from the nav).
// ---------------------------------------------------------------------------

/** SLW-NC-1: the provenance inputs, keyed the same as the nav selector so a
 *  repo's color/short-code is identical everywhere. `glyphMap` and `isLightTheme`
 *  always travel together. */
export interface NavProvenance {
  glyphMap: Map<string, string>;
  isLightTheme: boolean;
}

/** The per-repo color/short-code tint on a source-repo row (P3-AC-4). Renders
 *  nothing when the glyph map lacks the slug — the graceful no-tint fallback. */
export function SourceRepoTint({ slug, provenance }: { slug: string; provenance?: NavProvenance }) {
  const glyph = provenance?.glyphMap.get(slug);
  if (!glyph) return null;
  return (
    <RepoGlyph
      glyph={glyph}
      glyphColor={getRepoColor(slug, provenance!.isLightTheme)}
      slug={slug}
    />
  );
}

/**
 * SW-VE-5 (AC-1): the per-row scope badge. Renders the owner legibly:
 * - **Inherited** (owned by a parent team) → a dimmed marker naming the parent
 *   team (edits update that team's file). Wins over the repo tint.
 * - **Source repo** → the named repo tint (reuses `SourceRepoTint`).
 *
 * Null-safe: pass `resolveFileScope(...)` directly. `sourceRepo` is the raw slug
 * fallback so a source row with no team signal still tints exactly as before
 * VE-5 (no regression). Returns nothing when there is no inherited scope + no
 * slug (degrade — never a guessed badge).
 */
export function ScopeBadge({
  scope,
  sourceRepo,
  provenance,
}: {
  scope: FileScope | null;
  sourceRepo?: string;
  provenance?: NavProvenance;
}) {
  if (scope?.isInherited) {
    const parent = scope.sourceTeam ?? "parent team";
    return (
      <span
        className="inline-flex max-w-[7rem] flex-shrink-0 items-center gap-0.5 text-[10px] text-muted-foreground"
        role="img"
        aria-label={`Inherited from ${parent}`}
        data-testid="scope-badge-inherited"
      >
        <CornerLeftUp size={10} className="flex-shrink-0" aria-hidden />
        <span className="truncate">{parent}</span>
      </span>
    );
  }
  // A source-repo row: the named repo tint. Prefer the resolved slug, fall back
  // to the raw one so the pre-VE-5 tint is preserved.
  const slug = scope?.repoSlug ?? sourceRepo;
  if (slug) return <SourceRepoTint slug={slug} provenance={provenance} />;
  return null;
}

/**
 * SW-VE-5 (AC-2): the "Editing in <scope>" chrome chip. Presentational + pure.
 * Renders nothing without a scope. SW-XS-20: an inherited file is editable and
 * saves into its parent team, so the chip says where edits go.
 */
export function EditingInScope({
  scope,
  provenance,
}: {
  scope: FileScope | null;
  provenance?: NavProvenance;
}) {
  if (!scope) return null;
  return (
    <span
      className="inline-flex items-center gap-1 text-xs text-muted-foreground"
      data-testid="editing-in-scope"
    >
      {scope.isInherited ? (
        <>
          <CornerLeftUp size={11} className="flex-shrink-0" aria-hidden />
          From {scope.sourceTeam ?? "a parent team"} — edits save to{" "}
          {scope.sourceTeam ?? "that team"}
        </>
      ) : (
        <>
          Editing in {scopeLabel(scope)}
          {scope.repoSlug && <SourceRepoTint slug={scope.repoSlug} provenance={provenance} />}
        </>
      )}
    </span>
  );
}

/**
 * SW-XS-20: the inherited-file explainer shown while viewing a file that comes
 * from a parent team. The file stays editable — edits update the parent's file —
 * and `action` (e.g. "Copy into ‹team›") sits right after the line. Renders
 * nothing for a file that is not inherited.
 */
export function InheritedFileExplainer({
  scope,
  action,
}: {
  scope: FileScope | null;
  action?: React.ReactNode;
}) {
  if (!scope?.isInherited) return null;
  const parent = scope.sourceTeam ?? "a parent team";
  return (
    <div
      className="inline-flex items-center gap-1.5 text-xs text-muted-foreground"
      data-testid="inherited-file-explainer"
    >
      <CornerLeftUp size={12} className="flex-shrink-0" aria-hidden />
      <span>
        This file comes from <span className="font-medium">{parent}</span>. Changes you make here
        update it for {parent} and every team that inherits from it.
      </span>
      {action}
    </div>
  );
}
