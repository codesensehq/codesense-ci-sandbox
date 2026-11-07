// SPDX-License-Identifier: MIT
import { useLoadingDeadline } from "@/shared/hooks/use-loading-deadline";
import { useTeamContentContext } from "@/shared/hooks/use-team-content-context";
import { useCallback, useState } from "react";
import { toast } from "sonner";

import { useConfigStore, type RepoState } from "@/shared/stores/config-store";
import { resolveSourceRepoPath } from "@/shared/lib/resolve-source-repo-path";
import { resolveRepo } from "@/shared/lib/resolve-repo";
import { useViewerStore, promptIdentityFromRepoPath } from "@/shared/stores/viewer-store";
import { rowMatches, type SettingRowSpec } from "@/features/settings/lib/row-matches";
import { useFieldDraft } from "@/features/settings/lib/use-field-draft";
import { revealContextRepo } from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";

import { SectionHeader } from "./section-header";
import { SettingRow } from "./setting-row";

const REPO_KEYWORDS = [
  "repo",
  "repository",
  "main doc",
  "path",
  "paths",
  "context",
  "reveal",
  "worktree",
  "config",
];

export const REPO_SECTION_SPEC: SettingRowSpec = {
  key: "repo",
  label: "Repo",
  keywords: REPO_KEYWORDS,
};

interface RepoSectionProps {
  repo: RepoState | null;
  query: string;
  teamKey?: string;
}

export function RepoSection({ repo, query, teamKey }: RepoSectionProps) {
  // Substring-only — `rowMatches` defaults to non-fuzzy. With a long
  // keyword list, fuzzy walking surfaces false positives.
  const sectionVisible = rowMatches(REPO_SECTION_SPEC, query);

  return (
    <section className="pt-6" data-testid="settings-repo-section">
      <SectionHeader id="repo">Repo</SectionHeader>
      {sectionVisible && teamKey && <TeamMainDocRow key={teamKey} teamKey={teamKey} />}
      {sectionVisible &&
        (repo ? (
          <>
            {/* SW-CP-17: the ViewerRootRow + PipelineRootRow editing surfaces
                are retired (`viewerRoot` SW-CP-18, the prefix SW-XS-18). The
                viewer roots at `teamContentPath` and the scanner walks the
                whole content root. */}
            <MainDocRow repo={repo} />
            <RepoActionsSection repo={repo} />
          </>
        ) : (
          <p className="py-4 text-sm text-muted-foreground" data-testid="settings-repo-empty-state">
            No repos configured yet. Add a repo from the dashboard to configure settings.
          </p>
        ))}
    </section>
  );
}

// ---------- Main doc ----------

function MainDocRow({ repo }: { repo: RepoState }) {
  const commit = useCallback(
    async (next: string) => {
      await useConfigStore.getState().setMainDoc(repo.repoPath, next);
    },
    [repo.repoPath],
  );

  const { draft, onChange, onBlur, onKeyDown } = useFieldDraft({
    committed: repo.mainDoc,
    commit,
    label: "main doc",
    syncCommitted: true,
  });

  return (
    <SettingRow label="Main doc">
      <div className="space-y-1">
        <input
          type="text"
          placeholder="README.md"
          value={draft}
          onChange={onChange}
          onBlur={onBlur}
          onKeyDown={onKeyDown}
          aria-label="Main doc"
          data-testid="settings-repo-main-doc"
          className="h-7 w-full rounded border border-border bg-background px-2 text-xs font-mono outline-none focus:border-ring"
        />
        <p className="text-xs text-muted-foreground">
          Epic main doc filename convention. Leave empty for built-in fallback (README.md then
          index.md). Commits on blur.
        </p>
      </div>
    </SettingRow>
  );
}

// ---------- Actions ----------
//
// SS-9: RepoHealthSection was the legacy per-repo install/verify/repair/
// uninstall surface. Install actions retired with seamless-sync. SW-WT-8
// removed the "Validate worktree config" rehearsal surface (its only entry
// point was the unbuilt "Test my setup" UI), leaving the single surviving
// action: Reveal the source-code repo on disk.

function RepoActionsSection({ repo }: { repo: RepoState }) {
  return (
    <SettingRow label="Actions">
      <div className="flex flex-wrap gap-1.5">
        <button
          type="button"
          data-testid="settings-repo-reveal"
          onClick={() => {
            // F-6: reveal the source-code repo on disk. For synthetic
            // workspace repos with no attached clone, surface the AC-8
            // empty-state with auto-retry.
            //
            // R5C-P1: re-resolve `repo` from the store at fire time —
            // closing over the render-scope `repo` would see stale state
            // when the retry callback fires after a soft refresh updates
            // `workspaceRepos[repoPath]`.
            const targetRepoPath = repo.repoPath;
            const targetIdentity = promptIdentityFromRepoPath(targetRepoPath);
            const handler = () => {
              const current = resolveRepo(targetRepoPath);
              if (!current) return;
              const realPath = resolveSourceRepoPath(current);
              if (realPath === null) {
                // T.4 AC-3 / Phase 10 review (Round 2 MED #5): surface a
                // toast.error fallback when targetIdentity is null
                // (legacy non-synthetic repos). Pre-T.4 the prompt
                // surfaced unconditionally — silent return was a
                // regression.
                if (targetIdentity) {
                  useViewerStore
                    .getState()
                    .showNoClonePrompt(
                      targetIdentity.teamKey,
                      targetIdentity.sourceRepoSlug,
                      handler,
                    );
                } else {
                  toast.error("No local clone attached for this workspace repo");
                }
                return;
              }
              revealContextRepo(realPath).catch((e) => {
                logger.warn("settings", `revealContextRepo failed: ${e}`);
                toast.error("Failed to reveal context repo");
              });
            };
            handler();
          }}
          className="rounded px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        >
          Reveal
        </button>
      </div>
    </SettingRow>
  );
}

/** Context convention has its own owner; source actions keep their real RepoState. */
function TeamMainDocRow({ teamKey }: { teamKey: string }) {
  const context = useTeamContentContext(teamKey);
  const preference = useConfigStore((state) => state.teamMainDocPreferences?.[teamKey]);
  const committed = preference?.mainDoc ?? "";
  const [retry, setRetry] = useState(0);
  const expired = useLoadingDeadline(!preference, `${teamKey}:${retry}`);
  if (context.kind !== "ready") return null;
  if (!preference || expired)
    return (
      <SettingRow label="Team main doc">
        <div className="space-y-1">
          <p role="status" className="text-xs text-muted-foreground">
            {expired
              ? "Team preference lookup timed out. Files remain available with default detection."
              : "Loading team convention…"}
          </p>
          {expired && (
            <button
              type="button"
              className="text-xs underline"
              onClick={() => {
                setRetry((value) => value + 1);
                void useConfigStore
                  .getState()
                  .ensureTeamMainDocPreference(teamKey)
                  .catch((error) => toast.error(String(error)));
              }}
            >
              Retry team settings
            </button>
          )}
        </div>
      </SettingRow>
    );
  return context.kind === "ready" ? (
    <TeamMainDocField
      key={teamKey}
      teamKey={teamKey}
      committed={committed}
      preference={preference}
    />
  ) : null;
}
function TeamMainDocField({
  teamKey,
  committed,
  preference,
}: {
  teamKey: string;
  committed: string;
  preference: import("@/shared/lib/team-main-doc").TeamMainDocPreference | undefined;
}) {
  const commit = useCallback(
    (value: string) => useConfigStore.getState().setTeamMainDoc(teamKey, value),
    [teamKey],
  );
  const field = useFieldDraft({ committed, commit, label: "team main doc", syncCommitted: true });
  return (
    <SettingRow label="Team main doc">
      <div className="space-y-1">
        <input
          type="text"
          aria-label="Team main doc"
          data-testid="settings-team-main-doc"
          placeholder="README.md"
          value={field.draft}
          onChange={field.onChange}
          onBlur={field.onBlur}
          onKeyDown={field.onKeyDown}
          disabled={field.isCommitting}
          className="h-7 w-full rounded border border-border bg-background px-2 text-xs font-mono outline-none focus:border-ring"
        />
        <p className="text-xs text-muted-foreground">
          Epic summary filename for this team's context. Leave empty for README.md, then index.md.
          Commits on blur.
        </p>
        {preference?.kind === "conflict" && (
          <div className="space-y-1">
            <p role="status" className="text-xs text-muted-foreground">
              Source settings disagree ({preference.candidates.join(", ")}). Choose a team
              convention here; files remain available with default detection.
            </p>
            <button
              type="button"
              disabled={field.isCommitting}
              className="text-xs underline"
              onClick={() => {
                void commit("").catch((error) =>
                  toast.error(`Could not save team main doc: ${String(error)}`),
                );
              }}
            >
              Use default detection
            </button>
          </div>
        )}
        {preference?.kind === "unavailable" && (
          <p role="status" className="text-xs text-muted-foreground">
            Team preference could not be read: {preference.error}. Choose a convention here to retry
            saving.
          </p>
        )}
      </div>
    </SettingRow>
  );
}
