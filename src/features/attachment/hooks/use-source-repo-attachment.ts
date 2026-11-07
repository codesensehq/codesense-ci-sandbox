// SPDX-License-Identifier: MIT
import { useEffect, useRef } from "react";
import { toast } from "sonner";
import { toastFailure } from "@/shared/lib/with-error-toast";
import { persistent } from "@/components/ui/persistent-toast";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  resolveSourceRepoAttachment,
  saveSourceRepoAttachment,
  readSourceRepoOriginUrl,
  parseGitRemote,
} from "@/tauri/commands";
import type { Attachment } from "@/shared/types/domain";
import { commitRegistryEdit } from "@/shared/lib/commit-registry-edit";
import { deriveWorkspacesTracking } from "@/features/attachment/lib/derive-workspaces-tracking";
import { isSyntheticRepoPath } from "@/shared/lib/build-synthetic-repo";
import { getCurrentUser } from "@/shared/lib/get-current-user";
import { notifyLocalCloneAttached } from "@/features/attachment/lib/notify-clone-attached";
import { logger } from "@/shared/lib/logger";

/**
 * Orchestrates source repo attachment on repo selection:
 * 1. Silently records/updates the attachment.
 * 2. Derives workspace tracking matches.
 * 3. Shows a one-shot track prompt for untracked repos.
 */
export function useSourceRepoAttachment(activeRepoPath: string | null): void {
  const latestRepoPathRef = useRef<string | null>(null);

  useEffect(() => {
    latestRepoPathRef.current = activeRepoPath;

    if (!activeRepoPath || isSyntheticRepoPath(activeRepoPath)) return;

    const currentUser = getCurrentUser();
    if (!currentUser) return;

    const repoPath = activeRepoPath;
    const userId = currentUser.github_user_id;

    void runAttachmentFlow(repoPath, userId, latestRepoPathRef);

    // Cleanup: dismiss any track prompt for this repo on unmount or repo switch.
    return () => {
      toast.dismiss(`track-prompt:${repoPath}`);
    };
  }, [activeRepoPath]);
}

async function runAttachmentFlow(
  repoPath: string,
  userId: number,
  staleRef: React.RefObject<string | null>,
): Promise<void> {
  try {
    // Step 1: Resolve existing attachment.
    const existing = await resolveSourceRepoAttachment(repoPath, userId);
    if (staleRef.current !== repoPath) return;

    // Step 2: Read origin URL.
    let originUrl: string | null;
    try {
      originUrl = await readSourceRepoOriginUrl(repoPath);
    } catch (error) {
      toastFailure("Couldn't read this repository's origin remote", error);
      return;
    }
    if (staleRef.current !== repoPath) return;

    // Step 3: Parse before the first save so the credential binding always
    // matches the origin provider. Never fall back across providers.
    let parsed: Awaited<ReturnType<typeof parseGitRemote>> | null = null;
    if (originUrl) {
      try {
        parsed = await parseGitRemote(originUrl);
      } catch (error) {
        toastFailure("Couldn't recognize this repository's origin remote", error);
        return;
      }
    }
    if (staleRef.current !== repoPath) return;
    // Step 4: Silent record — save if new or URL changed.
    // SW-OB-3 (AC-2): a brand-new insert is confirmed with a toast below (once
    // owner/repo are parsed); a `remote_url`-changed update reuses this same
    // branch and MUST stay silent, so capture the insert-ness separately.
    const isNewInsert = !existing;
    const needsSave = !existing || existing.remote_url !== originUrl;
    if (needsSave) {
      const record: Attachment = {
        repo_path: repoPath,
        remote_url: originUrl,
        attached_at: null, // Backend owns on insert, preserves on update.
        tracking_prompt_dismissed_at: existing?.tracking_prompt_dismissed_at ?? null,
        credential_ref: null,
      };
      try {
        await saveSourceRepoAttachment(userId, record);
      } catch (e) {
        toastFailure("Couldn't record this repository", e);
        return;
      }
      // F-6: notify workspace-sync that attachments changed so any synthetic
      // workspace repos with matching (owner, repo) re-resolve their localPath
      // without requiring a workspace switch.
      useOrgStore.getState().bumpAttachmentsVersion();
      if (staleRef.current !== repoPath) return;
    }

    // SW-OB-3 (AC-2): confirm a brand-new attachment (the save above already
    // succeeded — a failure returned early). Only on a fresh insert, and only
    // when we can name the repo; re-attach / url-change stays silent.
    if (isNewInsert && parsed) {
      notifyLocalCloneAttached({ owner: parsed.owner, repo: parsed.repo });
    }

    // Step 5: Derive tracking matches.
    const orgs = useOrgStore.getState().orgs;
    const matches = deriveWorkspacesTracking(parsed, orgs);

    // Step 6: Decide whether to show prompt.
    const dismissedAt = existing?.tracking_prompt_dismissed_at ?? null;
    const activeOrgSlug = useOrgStore.getState().activeOrgSlug;
    const activeTeamName = useOrgStore.getState().activeTeamName;
    const activeOrg = orgs.find((o) => o.slug === activeOrgSlug);
    const hasTeam = activeOrg && activeOrg.teams.length > 0;

    const shouldPrompt =
      matches.length === 0 &&
      dismissedAt === null &&
      parsed !== null &&
      activeOrg != null &&
      hasTeam;

    if (!shouldPrompt) return;
    if (staleRef.current !== repoPath) return;

    // Step 7: Show toast prompt.
    // Use activeTeamName if it exists, otherwise fall back to first team.
    const teamName =
      activeTeamName && activeOrg!.teams.some((t) => t.name === activeTeamName)
        ? activeTeamName
        : activeOrg!.teams[0].name;
    const displayOwnerRepo = `${parsed!.owner}/${parsed!.repo}`;

    toast(
      `Track ${displayOwnerRepo} in ${activeOrg!.display_name}?`,
      persistent({
        id: `track-prompt:${repoPath}`,
        action: {
          label: "Accept",
          onClick: () => {
            // Re-read current state to avoid stale closure over org/permission.
            const currentOrg = useOrgStore.getState().orgs.find((o) => o.slug === activeOrgSlug);
            if (!currentOrg) return;
            void trackInActiveWorkspace(
              repoPath,
              userId,
              currentOrg,
              teamName,
              parsed!.owner,
              parsed!.repo,
              parsed!.provider,
              parsed!.host,
            );
          },
        },
        cancel: {
          label: "Not Now",
          onClick: () => {
            // Re-read current attachment to avoid stale closure.
            void dismissTrackPrompt(repoPath, userId);
          },
        },
      }),
    );
  } catch (e) {
    logger.warn("attachment", `runAttachmentFlow failed: ${e instanceof Error ? e.message : e}`);
  }
}

/** In-flight guard for the local track commit — prevents a double-click
 *  from firing two identical commits for the same org. */
const localTrackInFlight = new Set<string>();

async function trackInActiveWorkspace(
  repoPath: string,
  userId: number,
  org: { slug: string },
  teamName: string,
  owner: string,
  repo: string,
  provider: string,
  host: string,
): Promise<void> {
  const edit = {
    type: "AddSourceRepoToTeam" as const,
    team_name: teamName,
    owner,
    repo,
    provider,
    host,
  };

  // SW-OB-8: local-only commit regardless of `org.remote_url`. The
  // in-flight guard prevents a double-click from firing two identical
  // commits.
  if (localTrackInFlight.has(org.slug)) {
    toast.info("A track operation is already in progress.");
    return;
  }
  localTrackInFlight.add(org.slug);
  let success = false;
  try {
    await commitRegistryEdit(userId, org.slug, edit);
    toast.success("Tracked locally.");
    success = true;
  } catch (e) {
    toastFailure("Couldn't track this repo", e);
  } finally {
    localTrackInFlight.delete(org.slug);
  }

  if (success) {
    // PTS-2: the per-`(team, repo)` dir is no longer a content target —
    // install dirs are managed by `install_source_repo`, and the per-team
    // content dir is created lazily by Rust's capture writer when the
    // first story lands. The W.3 bootstrap-ensureDirectory call is retired.

    // Update in-memory org-store.
    useOrgStore
      .getState()
      .updateTeamSourceRepos(org.slug, teamName, { owner, repo, provider, host });

    // Persist dismissed_at to prevent re-prompt on restart.
    try {
      const current = await resolveSourceRepoAttachment(repoPath, userId);
      if (current) {
        // SW-FG-8 (AC-8): the `...current` spread carries `credential_ref`
        // — keep the spread so this update never nulls the binding.
        await saveSourceRepoAttachment(userId, {
          ...current,
          tracking_prompt_dismissed_at: new Date().toISOString(),
        });
      }
    } catch {
      // Non-critical — prompt suppression is a UX enhancement.
    }
  }
}

async function dismissTrackPrompt(repoPath: string, userId: number): Promise<void> {
  try {
    // Re-read current attachment to get latest state (avoids stale closure).
    const current = await resolveSourceRepoAttachment(repoPath, userId);
    const record: Attachment = {
      repo_path: repoPath,
      remote_url: current?.remote_url ?? null,
      attached_at: current?.attached_at ?? null,
      tracking_prompt_dismissed_at: new Date().toISOString(),
      // SW-FG-8 (AC-8): carry the existing binding — this update path builds a
      // fresh record, so without this it would null `credential_ref`.
      credential_ref: current?.credential_ref ?? null,
    };
    await saveSourceRepoAttachment(userId, record);
  } catch (e) {
    toastFailure("Couldn't dismiss the prompt", e);
  }
}
