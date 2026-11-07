// SPDX-License-Identifier: MIT
import { type ReactNode } from "react";
import { FolderOpen, Users, Building2, LayoutPanelLeft } from "lucide-react";

import { useCurrentUser } from "@/shared/lib/get-current-user";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { openSourceRepos } from "@/features/dashboard/source-repos-virtual-tab";
import { openManageTeams } from "@/features/shell/manage-teams-virtual-tab";
import { openWorkspaces } from "@/features/org/lib/open-workspaces";
import { buildTeamKey } from "@/shared/lib/team-key";
import { workspaceReposForTeam } from "@/shared/lib/build-synthetic-repo";
import { teamHasSatisfiedClone } from "@/features/attachment/lib/team-clone-gate";
import { TEAM_EXPLAINER } from "@/shared/lib/onboarding-copy";

import { EmptyState } from "@/shared/components/empty-state";
import { LoadingState } from "@/shared/components/spinner";

/** T.7: first-action empty state rendered in place of the dashboard
 *  workspace bar when there is nothing for the user to do yet.
 *
 *  Branches (D-2; SW-OB-7 opened access — existence, not membership). It never
 *  renders nothing (SW-FU-B2: this is the floor under a blank content area):
 *  - currentUser still resolving → a loading state.
 *  - no active org → "Pick a workspace or create a new one" → the Workspaces tab.
 *  - active org, but `activeTeamName` names no existing team (unset or a
 *    stale/deleted name) → "Pick a team in {orgName}" → team switcher.
 *  - active team exists, zero source_repos → "Add a source repo to
 *    {teamName}" → lifted `AttachRepoDialog` via `openAttachRepoDialog`.
 *  - SW-OB-3: active team tracks repos but none has an attached local clone →
 *    "Attach a local clone for {teamName}" → reuses `AttachLocalCloneDialog`.
 *
 *  Mounted at `App.tsx` top level when there is no valid active team OR the
 *  active team has no attached clone (SW-OB-9 + SW-OB-3). The component is
 *  store-driven — no props. */
export function TeamWorkspaceEmptyState() {
  const currentUser = useCurrentUser();
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const activeTeamName = useOrgStore((s) => s.activeTeamName);
  const orgs = useOrgStore((s) => s.orgs);
  const workspaceRepos = useOrgStore((s) => s.workspaceRepos);

  if (currentUser === null) {
    return (
      <FirstRunShell>
        <LoadingState
          label="Loading local profile…"
          detail="CodeSense is reading the selected local profile."
          escape={{ label: "Reload", onClick: () => window.location.reload() }}
        />
      </FirstRunShell>
    );
  }

  const activeOrg = activeOrgSlug ? (orgs.find((o) => o.slug === activeOrgSlug) ?? null) : null;

  // SW-UX (app-flow redesign): each rung sits in the shared first-run shell (a
  // top-anchored max-w column). Home Base is retired, so there is no "back to
  // home base" affordance any more.
  const shell = (content: ReactNode) => <FirstRunShell>{content}</FirstRunShell>;

  if (activeOrgSlug === null) {
    return shell(<NoOrgState />);
  }

  // SW-OB-7: open access — the active team just has to EXIST in the org
  // (no membership gate). Keep the iter-1 HIGH specificity: key on the
  // SPECIFIC `activeTeamName`, not org-wide, so a stale/deleted team name
  // (which won't match any `t.name`) still routes to the no-team state and
  // gets the team-switcher CTA. A foreign-but-existing active team is now
  // valid (the user may work on it), so it falls through to the source-repo
  // check. Existence is read over `org.teams` DIRECTLY — never a curated
  // "visible" subset — so SW-OB-9's local team-hide can't make this branch
  // believe the org has no teams.
  const isOnActiveTeam =
    activeOrg !== null &&
    activeTeamName !== null &&
    activeOrg.teams.some((t) => t.name === activeTeamName);

  if (!isOnActiveTeam) {
    // `orgHasTeams` selects between the "pick a team" state (teams present)
    // and the "no teams yet → create in Settings" state. Open access keys
    // this on team existence, not the user's memberships.
    const orgHasTeams = activeOrg !== null && activeOrg.teams.length > 0;
    return shell(
      <NoTeamInOrgState
        orgName={activeOrg?.display_name ?? activeOrgSlug}
        orgHasTeams={orgHasTeams}
      />,
    );
  }

  // SW-UX (app-flow redesign): the multi-rung ladder is collapsed. There is no
  // separate "track a repo → then attach a clone → then set up" staircase any
  // more — attaching a source repo (a local folder or a URL) via the ONE two-path
  // dialog IS the whole action. So whether the team tracks zero repos OR tracks
  // repos with nothing attached locally, the honest single state is the same:
  // "add a source repo". (The old NoAttachedClone clone-gate and the setup-checklist
  // rungs are removed.)
  const teamKey = buildTeamKey(activeOrgSlug, activeTeamName);
  const teamRepos = workspaceReposForTeam(workspaceRepos, teamKey);
  if (teamRepos.length === 0 || !teamHasSatisfiedClone(teamRepos)) {
    return shell(<NoSourceReposState teamKey={teamKey} teamName={activeTeamName} />);
  }

  // SW-FU-B2: a usable team whose pane isn't open (App routes here when the
  // active team is missing from `openedTeams`). Opening it mounts the pane.
  return shell(<TeamNotOpenState teamKey={teamKey} teamName={activeTeamName} />);
}

/** SW-FU-B2: open the team's pane. Every action on this floor runs it first, so
 *  a tab it opens lands in a mounted pane instead of an unmounted one. */
function openTeamPane(teamKey: string) {
  useViewerStore.getState().openTeam(teamKey);
}

function TeamNotOpenState({ teamKey, teamName }: { teamKey: string; teamName: string }) {
  return (
    <EmptyState
      size="page"
      icon={<LayoutPanelLeft />}
      title={`Open ${teamName}`}
      body="This team isn't open in this window yet."
      action={{ label: "Open team", onClick: () => openTeamPane(teamKey) }}
    />
  );
}

/** The shared first-run shell: a scrollable, top-anchored `max-w` column.
 *
 *  SW-OC-B9: the workspace-name header is dropped. The persistent title bar's
 *  `IdentityMenu` already shows the org (workspace) name at all times, so a
 *  second heading here triplicated the org name (title bar + this header + the
 *  left-nav pill) — visual noise on first launch. Identity lives in the title bar;
 *  this shell just frames the single first-run action. */
function FirstRunShell({ children }: { children: ReactNode }) {
  return (
    <div className="flex h-full flex-col overflow-hidden bg-background">
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex w-full max-w-3xl flex-col gap-6 px-6 py-8">{children}</div>
      </div>
    </div>
  );
}

function NoOrgState() {
  // SW-FU-B7: both actions open the Workspaces TAB (in the no-team tab host) —
  // not the retired create/join overlay.
  return (
    <EmptyState
      size="page"
      icon={<Building2 />}
      title="Open a context repo"
      body="Connect your team's context repo, or create a new one."
      action={{
        label: "Open context repos",
        onClick: () => openWorkspaces("home"),
      }}
      secondaryAction={JOIN_YOUR_TEAM}
    />
  );
}

/** SW-FU-B6: every no-team floor offers joining your team, straight to the join
 *  form in the Workspaces tab (the no-team tab host when there is no team). */
const JOIN_YOUR_TEAM = { label: "Connect a context repo…", onClick: () => openWorkspaces("join") };

function NoTeamInOrgState({ orgName, orgHasTeams }: { orgName: string; orgHasTeams: boolean }) {
  // SW-OB-7: open access — copy keys on whether the org has any teams, not on
  // the user's memberships. The CTAs open the team-management tab (create or
  // browse), which folds create + visibility + archive into one surface.
  if (!orgHasTeams) {
    return (
      <EmptyState
        size="page"
        icon={<Users />}
        title={`No teams in ${orgName} yet`}
        body={`${TEAM_EXPLAINER} Create your first team to start working.`}
        action={{
          label: "Create a team",
          onClick: () => openManageTeams("create"),
        }}
        secondaryAction={JOIN_YOUR_TEAM}
      />
    );
  }
  // SW-OB-9 (AC-6): the org HAS teams but none is active — reachable when the
  // user hides the last shown team. Route to the team-management tab to un-hide /
  // pick teams — the no-dead-end guarantee: every empty shown-set has a re-add surface.
  return (
    <EmptyState
      size="page"
      icon={<Users />}
      title={`Pick a team in ${orgName}`}
      body={`${TEAM_EXPLAINER} Pick a team to start working.`}
      action={{
        label: "Browse all teams",
        onClick: () => openManageTeams(),
      }}
      secondaryAction={JOIN_YOUR_TEAM}
    />
  );
}

function NoSourceReposState({ teamKey, teamName }: { teamKey: string; teamName: string }) {
  // SW-UX (app-flow redesign): repo management is first-class ON the team, not
  // buried in Settings. Adding a repo (local folder or URL) is the one action —
  // it opens the source-repos tab with the add reserve expanded.
  return (
    <EmptyState
      size="page"
      icon={<FolderOpen />}
      title={`Add a source repo to ${teamName}`}
      body="Point CodeSense at your code — a local folder on this machine, or a remote URL — to start working on stories and opening terminals."
      action={{
        label: "Add source repo",
        onClick: () => {
          openTeamPane(teamKey);
          openSourceRepos("add");
        },
      }}
    />
  );
}
