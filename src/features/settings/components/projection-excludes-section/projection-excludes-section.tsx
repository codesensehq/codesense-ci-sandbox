// SPDX-License-Identifier: MIT
import { useState } from "react";
import { X } from "lucide-react";

import { setProjectionExcludes } from "@/tauri/commands";
import { rowMatches, type SettingRowSpec } from "@/features/settings/lib/row-matches";
import { useOrgStore } from "@/shared/stores/org-store";
import { useActiveTeamConfigTarget } from "@/features/settings/hooks/use-active-team-config-target";
import { useProjectionExcludes } from "@/features/settings/hooks/use-projection-excludes";
import { useSubmitAction } from "@/shared/hooks/use-submit-action";
import { SectionHeader } from "../section-header";
import { SettingsCard } from "../workspace-section/settings-card";

/** SW-CP-28 · The nav/search spec for the Excluded paths section. */
export const PROJECTION_EXCLUDES_SECTION_SPEC: SettingRowSpec = {
  key: "projection-excludes",
  label: "Excluded paths",
  keywords: [
    "projection",
    "project",
    "exclude",
    "excludes",
    "excluded",
    "ignore",
    "ignored",
    "secret",
    "secrets",
    "credential",
    "credentials",
    "hydrate",
    "worktree",
    "env",
    ".env",
    "glob",
  ],
};

// ---------- Pure exclude model (exported for tests) ----------
//
// The single source of truth is the `patterns` array as stored in
// `projection-excludes.yml`: a plain glob is an ADDED exclude; a leading `!` is a
// NEGATION (allow) of a default. The UI is a lossless view over this array — stray
// negations of non-defaults round-trip untouched because we mutate the array
// directly rather than reconstructing it.

/** Is `def` (a default glob) currently excluded — i.e. NOT negated by `!def`? */
export function isDefaultExcluded(patterns: readonly string[], def: string): boolean {
  return !patterns.includes(`!${def}`);
}

/** Toggle a default's excluded state: add/remove its `!`-negation. */
export function toggleDefault(patterns: readonly string[], def: string): string[] {
  const neg = `!${def}`;
  return patterns.includes(neg) ? patterns.filter((p) => p !== neg) : [...patterns, neg];
}

/** The team-added custom excludes (plain globs, not `!`-negations). */
export function customPatterns(patterns: readonly string[]): string[] {
  return patterns.filter((p) => !p.startsWith("!"));
}

/** Append a custom exclude glob (idempotent — a duplicate is a no-op). */
export function addCustom(patterns: readonly string[], glob: string): string[] {
  return patterns.includes(glob) ? [...patterns] : [...patterns, glob];
}

/** Remove a custom exclude glob. */
export function removeCustom(patterns: readonly string[], glob: string): string[] {
  return patterns.filter((p) => p !== glob);
}

interface ProjectionExcludesSectionProps {
  query: string;
}

/**
 * SW-CP-28 · Settings → Excluded paths (team-scoped; stored in
 * `projection-excludes.yml`). Lets a team choose which content paths stay in
 * `content/` but are never copied into a workspace: not at an agent launch, not
 * by an explicit hydrate, and a publish never writes them back. The credential
 * defaults (`.env`, `id_rsa`, …) render as pre-checked toggles a team can uncheck
 * to allow; custom globs (e.g. a tfstate pattern) are added below. Edits write the
 * team config and apply from the next launch or explicit operation.
 */
export function ProjectionExcludesSection({ query }: ProjectionExcludesSectionProps) {
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const stateLoaded = useOrgStore((s) => s.stateLoaded);
  const target = useActiveTeamConfigTarget();

  const visible = rowMatches(PROJECTION_EXCLUDES_SECTION_SPEC, query);
  if (!visible) return null;

  return (
    <section className="pt-6" data-testid="settings-projection-excludes-section">
      <SectionHeader id="projection-excludes">Excluded paths</SectionHeader>
      <p className="py-2 text-sm text-muted-foreground">
        Files matching these globs stay in <code>content/</code> but are never copied into a
        workspace, and a publish never writes them back. The whole team shares this list.
      </p>
      <Body
        activeOrgSlug={activeOrgSlug}
        stateLoaded={stateLoaded}
        team={target.team}
        orgRoot={target.orgRoot}
      />
    </section>
  );
}

function Body({
  activeOrgSlug,
  stateLoaded,
  team,
  orgRoot,
}: {
  activeOrgSlug: string | null;
  stateLoaded: boolean;
  team: string | null;
  orgRoot: string;
}) {
  if (stateLoaded && activeOrgSlug === null) {
    return (
      <p
        className="py-4 text-sm text-muted-foreground"
        data-testid="settings-projection-excludes-no-active"
      >
        No active workspace — use the workspace switcher to pick one.
      </p>
    );
  }
  if (team === null || orgRoot === "") {
    return (
      <p
        className="py-4 text-sm text-muted-foreground"
        data-testid="settings-projection-excludes-no-team"
      >
        Select a team to edit its excluded paths.
      </p>
    );
  }
  return <Editor orgRoot={orgRoot} team={team} />;
}

function Editor({ orgRoot, team }: { orgRoot: string; team: string }) {
  const [nonce, setNonce] = useState(0);
  const { defaults, patterns, loading } = useProjectionExcludes(orgRoot, team, nonce);
  const { submitting, submit } = useSubmitAction();

  // `draft ?? patterns` — a null draft means "in sync with backend"; edits fork a
  // working copy that a successful save clears (then the nonce bump reloads).
  const [draft, setDraft] = useState<string[] | null>(null);
  const working = draft ?? patterns;
  const dirty = draft !== null;

  const [customInput, setCustomInput] = useState("");

  const onSave = () => {
    void submit(
      async () => {
        await setProjectionExcludes(orgRoot, team, working);
        setDraft(null);
        setNonce((n) => n + 1);
      },
      {
        success: "Saved excluded paths",
        errorPrefix: "Failed to save excluded paths",
      },
    );
  };

  const onAddCustom = () => {
    const glob = customInput.trim();
    if (!glob) return;
    setDraft(addCustom(working, glob));
    setCustomInput("");
  };

  if (loading) {
    return (
      <p
        className="py-4 text-sm text-muted-foreground"
        data-testid="settings-projection-excludes-loading"
      >
        Loading…
      </p>
    );
  }

  const custom = customPatterns(working);

  return (
    <div className="mt-4 flex flex-col gap-4">
      <SettingsCard title="Credential defaults" data-testid="projection-excludes-defaults-card">
        <p className="mb-3 text-xs text-muted-foreground">
          Sensible defaults. Uncheck one to let it project (e.g. an engineer who wants{" "}
          <code>.env</code> in their worktree).
        </p>
        <p
          className="mb-3 text-xs text-muted-foreground"
          data-testid="projection-excludes-case-hint"
        >
          Excludes match any letter case, so <code>.ENV</code> is excluded like <code>.env</code>.
          Unchecking a default lets through only the exact spelling shown.
        </p>
        <ul className="flex flex-col gap-1.5">
          {defaults.map((def) => {
            const excluded = isDefaultExcluded(working, def);
            return (
              <li key={def} className="flex items-center gap-2">
                <input
                  type="checkbox"
                  id={`exclude-default-${def}`}
                  checked={excluded}
                  data-testid={`projection-excludes-default-${def}`}
                  onChange={() => setDraft(toggleDefault(working, def))}
                  className="h-4 w-4"
                />
                <label htmlFor={`exclude-default-${def}`} className="font-mono text-sm">
                  {def}
                </label>
              </li>
            );
          })}
        </ul>
      </SettingsCard>

      <SettingsCard
        title="Custom excludes"
        count={custom.length}
        data-testid="projection-excludes-custom-card"
      >
        <div className="mb-3 flex items-center gap-2">
          <input
            type="text"
            value={customInput}
            placeholder="e.g. **/*.tfstate"
            aria-label="Add a custom exclude glob"
            data-testid="projection-excludes-custom-input"
            onChange={(e) => setCustomInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                onAddCustom();
              }
            }}
            className="flex-1 rounded border border-border bg-transparent px-2 py-1 font-mono text-sm outline-none focus:border-ring"
          />
          <button
            type="button"
            onClick={onAddCustom}
            data-testid="projection-excludes-custom-add"
            className="rounded border border-border px-2 py-1 text-sm hover:bg-accent hover:text-accent-foreground"
          >
            Add
          </button>
        </div>
        <p className="mb-3 text-xs text-muted-foreground">
          A custom exclude matches any letter case. A <code>!</code> pattern in{" "}
          <code>projection-excludes.yml</code> brings a path back only in the exact case written.
        </p>
        {custom.length === 0 ? (
          <p className="text-xs text-muted-foreground">No custom excludes.</p>
        ) : (
          <ul className="flex flex-col gap-1">
            {custom.map((glob) => (
              <li key={glob} className="flex items-center justify-between gap-2">
                <span className="font-mono text-sm">{glob}</span>
                <button
                  type="button"
                  aria-label={`Remove ${glob}`}
                  data-testid={`projection-excludes-remove-${glob}`}
                  onClick={() => setDraft(removeCustom(working, glob))}
                  className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
                >
                  <X size={14} aria-hidden="true" />
                </button>
              </li>
            ))}
          </ul>
        )}
      </SettingsCard>

      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={onSave}
          disabled={!dirty || submitting}
          data-testid="projection-excludes-save"
          className="rounded bg-primary px-3 py-1.5 text-sm text-primary-foreground disabled:opacity-50"
        >
          {submitting ? "Saving…" : "Save"}
        </button>
        {dirty && !submitting && (
          <button
            type="button"
            onClick={() => setDraft(null)}
            data-testid="projection-excludes-reset"
            className="text-sm text-muted-foreground hover:text-foreground"
          >
            Reset
          </button>
        )}
      </div>
    </div>
  );
}
