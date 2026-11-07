// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState } from "react";
import { Check, Circle, FolderGit2, PackageCheck } from "lucide-react";
import { toast } from "sonner";

import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { buildTeamKey } from "@/shared/lib/team-key";
import { workspaceReposForTeam, parseSyntheticRepoKey } from "@/shared/lib/build-synthetic-repo";
import { sourceRepoRemoteKey } from "@/shared/lib/source-repo-key";
import { isCloneAttached } from "@/features/attachment/lib/team-clone-gate";
import { useAttachmentsByRemote } from "@/features/attachment/hooks/use-attachments-by-remote";
import { AttachLocalCloneDialog } from "@/features/attachment/components/attach-local-clone-dialog";
import { removeSourceRepoFromTeam } from "@/tauri/commands";
import { getCurrentUser } from "@/shared/lib/get-current-user";
import { CONTEXT_LANDS_ATTACHED, CONTEXT_LANDS_MANAGED } from "@/shared/lib/onboarding-copy";
import { InlineAlert } from "@/components/ui/inline-alert";
import { AppButton } from "@/shared/components/app-button";
import { EmptyState } from "@/shared/components/empty-state";
import { TabHeader, TabSection, TabSurface } from "@/shared/components/tab-surface";
import { openWorkspaces } from "@/features/org/lib/open-workspaces";
import { AddRepoForm } from "./add-repo-form";

/**
 * SW-OC-8 — the Source-repos tab is the ONE simple, prominent home for getting
 * code into a team. Three plainly-labeled paths live IN the tab content (not in
 * Settings, not a nested dialog dig):
 *
 *   1. Add from GitHub (URL)         — `AddRepoForm` url mode.
 *   2. Attach a local clone (folder) — `AddRepoForm` local mode / per-repo attach.
 *   3. Use a CodeSense-managed workspace — the `AttachLocalCloneDialog` "Clone for
 *      me" (managed) path, surfaced as a per-repo one-click tab action.
 *
 * The add section is a first-class panel at the top (no cramped reserve pane).
 * Each tracked repo shows its working-copy state — attached / managed /
 * not-attached — with a one-click "Attach local clone" or "Use managed
 * workspace" initiated FROM the tab. The choice + entry live here; a folder
 * picker (via `AttachLocalCloneDialog`) may still open for the pick step. This is
 * a surfacing/restructure over the existing attach commands — no new backend.
 */

interface RepoRow {
  key: string;
  label: string;
  slug: { owner: string; repo: string; provider: string; host: string } | null;
  /** attached (your own clone) / managed (codesense-cloned) / not-attached. */
  state: "attached" | "managed" | "not-attached";
  localPath: string | null;
}

export function ManageReposTab() {
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const activeTeamName = useOrgStore((s) => s.activeTeamName);
  const workspaceRepos = useOrgStore((s) => s.workspaceRepos);
  const removeTeamSourceRepo = useOrgStore((s) => s.removeTeamSourceRepo);
  const attachmentsByRemote = useAttachmentsByRemote();
  const intent = useUiStore((s) => s.sourceReposIntent);
  const setSourceReposIntent = useUiStore((s) => s.setSourceReposIntent);

  const [removing, setRemoving] = useState<string | null>(null);
  // A failed remove says so on its own row, not in a toast (the result of an
  // action on the visible tab).
  const [removeFailure, setRemoveFailure] = useState<{
    key: string;
    teamKey: string;
    error: unknown;
  } | null>(null);
  // Per-repo attach dialog target: which tracked repo the dialog acts on.
  const [attachTarget, setAttachTarget] = useState<{
    slug: NonNullable<RepoRow["slug"]>;
    label: string;
  } | null>(null);

  // React to the open intent. `"add"` is retained for callers that expect the add
  // affordance emphasised; the add section is now always visible, so this is a
  // no-op beyond clearing the intent so it doesn't re-fire.
  useEffect(() => {
    if (intent !== null) setSourceReposIntent(null);
  }, [intent, setSourceReposIntent]);

  const teamKey =
    activeOrgSlug && activeTeamName ? buildTeamKey(activeOrgSlug, activeTeamName) : null;

  const rows = useMemo<RepoRow[]>(() => {
    if (!teamKey) return [];
    return workspaceReposForTeam(workspaceRepos, teamKey).map((r) => {
      const parsed = parseSyntheticRepoKey(r.repoPath);
      const slug = parsed
        ? {
            owner: parsed.owner,
            repo: parsed.repo,
            provider: "github",
            host: parsed.host ?? "github.com",
          }
        : null;
      let managed = false;
      if (slug) {
        try {
          managed = attachmentsByRemote.get(sourceRepoRemoteKey(slug))?.managed === true;
        } catch {
          managed = false;
        }
      }
      const attached = isCloneAttached(r);
      return {
        key: r.repoPath,
        label: parsed ? `${parsed.owner}/${parsed.repo}` : r.repoName,
        slug,
        state: attached ? (managed ? "managed" : "attached") : "not-attached",
        localPath: r.localPath ?? null,
      };
    });
  }, [teamKey, workspaceRepos, attachmentsByRemote]);

  const detach = async (row: RepoRow) => {
    if (!row.slug || !activeOrgSlug || !activeTeamName) return;
    const currentUser = getCurrentUser();
    if (!currentUser) return;
    setRemoving(row.key);
    setRemoveFailure(null);
    try {
      await removeSourceRepoFromTeam({
        githubUserId: currentUser.github_user_id,
        orgSlug: activeOrgSlug,
        teamName: activeTeamName,
        owner: row.slug.owner,
        repo: row.slug.repo,
        provider: row.slug.provider,
        host: row.slug.host,
      });
      removeTeamSourceRepo(activeOrgSlug, activeTeamName, row.slug);
      useOrgStore.getState().bumpAttachmentsVersion();
      toast.success(`Removed ${row.label} from ${activeTeamName}`);
    } catch (e) {
      setRemoveFailure({
        key: row.key,
        teamKey: buildTeamKey(activeOrgSlug, activeTeamName),
        error: e,
      });
    } finally {
      setRemoving(null);
    }
  };

  if (!teamKey || !activeOrgSlug || !activeTeamName) {
    return (
      <TabSurface testId="source-repos-tab">
        <EmptyState
          icon={<FolderGit2 />}
          title="No team open"
          body="Open a team to manage its source repos."
          action={{ label: "Open context repos", onClick: () => openWorkspaces("home") }}
        />
      </TabSurface>
    );
  }

  return (
    <TabSurface testId="source-repos-tab">
      <TabHeader
        title={`Source repos · ${activeTeamName}`}
        description="The code this team works over. Add a repo and get a working copy — everything you need is right here."
      >
        <p data-testid="connect-repo-why" className="text-sm text-muted-foreground">
          CodeSense needs read access to run agents against your code. It stays on your machine.
        </p>
      </TabHeader>

      {/* SW-OC-8 S1/S3: first-class "add a source repo" panel — the three paths,
          plainly labeled, prominent (not a cramped reserve pane). */}
      <TabSection
        card
        testId="add-repo-panel"
        title="Add a source repo"
        description="Two ways to get code into this team:"
      >
        <ul className="flex flex-col gap-1.5 text-xs text-muted-foreground">
          <li data-testid="add-path-github">
            <span className="font-medium text-foreground">Add from GitHub</span> — paste a repo URL
            to track it on the team.
          </li>
          <li data-testid="add-path-local">
            <span className="font-medium text-foreground">Attach a local clone</span> — point
            CodeSense at a folder you already have on disk.
          </li>
        </ul>

        {/* Add-from-GitHub (URL) + Attach-a-local-clone (folder) — reuse the
            two-mode AddRepoForm, now first-class at the top of the tab. */}
        <AddRepoForm
          target={{ orgSlug: activeOrgSlug, teamName: activeTeamName }}
          onDone={() => {
            useOrgStore.getState().bumpAttachmentsVersion();
          }}
        />
      </TabSection>

      {/* Tracked repos — per-repo working-copy state + one-click attach. */}
      <TabSection title="Tracked repos">
        {rows.length === 0 ? (
          <EmptyState
            testId="manage-repos-empty"
            icon={<FolderGit2 />}
            title="No source repos yet"
            body="Add one above to start working."
          />
        ) : (
          <ul className="flex flex-col gap-1.5">
            {rows.map((row) => (
              <li
                key={row.key}
                data-testid={`manage-repos-row-${row.label}`}
                data-state={row.state}
                className="flex flex-col gap-2 rounded-md border border-border px-3 py-2"
              >
                <div className="flex items-center gap-3">
                  <FolderGit2
                    size={16}
                    className="shrink-0 text-muted-foreground"
                    aria-hidden="true"
                  />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm font-medium">{row.label}</div>
                    <div
                      className="flex items-center gap-1 text-xs text-muted-foreground"
                      data-testid={`manage-repos-state-${row.label}`}
                    >
                      {row.state === "managed" ? (
                        <>
                          <PackageCheck
                            size={12}
                            className="text-emerald-600 dark:text-emerald-400"
                          />
                          <span className="truncate">Managed clone · {row.localPath}</span>
                        </>
                      ) : row.state === "attached" ? (
                        <>
                          <Check size={12} className="text-green-600 dark:text-green-400" />
                          <span className="truncate">Attached · {row.localPath}</span>
                        </>
                      ) : (
                        <>
                          <Circle size={12} />
                          <span>Not attached — no working copy yet</span>
                        </>
                      )}
                    </div>
                    {/* SW-LN-29 (AC-1): where the team context lands for this copy. */}
                    {row.state !== "not-attached" && (
                      <p
                        data-testid={`manage-repos-context-${row.label}`}
                        className="mt-0.5 text-xs text-muted-foreground"
                      >
                        {row.state === "managed" ? CONTEXT_LANDS_MANAGED : CONTEXT_LANDS_ATTACHED}
                      </p>
                    )}
                  </div>

                  {/* SW-OC-8 S2: per-repo one-click attach, initiated FROM the tab. */}
                  {row.state === "not-attached" && row.slug && (
                    <AppButton
                      type="button"
                      variant="outline"
                      size="xs"
                      className="shrink-0"
                      data-testid={`attach-local-${row.label}`}
                      onClick={() => setAttachTarget({ slug: row.slug!, label: row.label })}
                    >
                      Attach local clone
                    </AppButton>
                  )}

                  <AppButton
                    type="button"
                    variant="destructive"
                    size="xs"
                    data-testid={`manage-repos-remove-${row.label}`}
                    disabled={removing === row.key || !row.slug}
                    onClick={() => detach(row)}
                  >
                    {removing === row.key ? "Removing…" : "Remove"}
                  </AppButton>
                </div>
                {removeFailure?.key === row.key && removeFailure.teamKey === teamKey && (
                  <InlineAlert
                    severity="error"
                    data-testid={`manage-repos-remove-error-${row.label}`}
                    title={`Couldn't remove ${row.label}`}
                    error={removeFailure.error}
                    action={{
                      label: "Try again",
                      onClick: () => void detach(row),
                      disabled: removing === row.key || !row.slug,
                      testId: `manage-repos-remove-retry-${row.label}`,
                    }}
                  />
                )}
              </li>
            ))}
          </ul>
        )}
      </TabSection>

      {/* Per-repo attach flow — the SAME dialog Settings uses (reuses the existing
          attach/clone commands). The CHOICE + entry live in the tab above; this
          modal handles the folder-pick / clone step. */}
      {attachTarget && (
        <AttachLocalCloneDialog
          owner={attachTarget.slug.owner}
          repo={attachTarget.slug.repo}
          provider={attachTarget.slug.provider}
          host={attachTarget.slug.host}
          displayLabel={attachTarget.label}
          open={true}
          onOpenChange={(open) => {
            if (!open) setAttachTarget(null);
          }}
        />
      )}
    </TabSurface>
  );
}
