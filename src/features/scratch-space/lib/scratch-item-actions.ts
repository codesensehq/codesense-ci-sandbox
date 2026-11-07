// SPDX-License-Identifier: MIT
import {
  ALL_ORGS_LABEL,
  SCRATCH_SPACE_LABEL,
  allOrgsContextId,
  scratchSpaceContextId,
  scratchSpaceInLabel,
  teamAllContextId,
  type Context,
} from "@/shared/lib/context-model";
import type { ScratchInertKind } from "@/tauri/commands";

/**
 * What can be done with one item of the scratch space, and where each verb sends
 * it. Pure: the Scratch space tab rows and the worktree file tree's menu share it,
 * and the move/copy/remove itself runs through the same sheets and backend as every
 * other context move (so the gates match the MCP tools step for step).
 */

/** One scratch file, wherever it is shown. */
export interface ScratchItem {
  /** The org the item is kept for — or, for an all-orgs item, the org it is being
   *  looked at in (the target of "Only this org"). `null` when no org is open. */
  orgSlug: string | null;
  /** Its path within the scratch space (where it applies in a team's files). */
  logical: string;
  /** Kept for one org, or in the all-orgs folder. */
  where: "org" | "allOrgs";
  /** The kind it would apply as in every org; `null` when it can't go there. */
  inertKind: ScratchInertKind | null;
  /** The team "Share with" proposes first (the open team, or the worktree's). */
  defaultTeam: string | null;
}

export type ScratchVerb = "share" | "copyToOrg" | "useInAllOrgs" | "onlyThisOrg" | "remove";

export interface ScratchVerbRow {
  verb: ScratchVerb;
  label: string;
  /** Why the verb can't run for this item — shown under it, never hidden. */
  disabledReason?: string;
  destructive?: boolean;
}

/** What the verbs need to know about the rest of the app. */
export interface ScratchVerbFacts {
  /** Teams of the item's org that can receive it. */
  teamCount: number;
  /** Other orgs on this Mac a copy can go to. */
  otherOrgCount: number;
}

export const NOT_INERT_REASON =
  "Only skills, prompts, snippets and templates can apply in every org";

/** The verbs for an item, in menu order. */
export function scratchVerbs(item: ScratchItem, facts: ScratchVerbFacts): ScratchVerbRow[] {
  const noOrg = item.orgSlug === null ? "Open an org first" : undefined;
  const share: ScratchVerbRow = {
    verb: "share",
    label: item.defaultTeam ? `Share with ${item.defaultTeam}…` : "Share with a team…",
    disabledReason: noOrg ?? (facts.teamCount === 0 ? "This org has no team yet" : undefined),
  };
  const remove: ScratchVerbRow = { verb: "remove", label: "Remove…", destructive: true };
  if (item.where === "allOrgs") {
    return [share, { verb: "onlyThisOrg", label: "Only this org", disabledReason: noOrg }, remove];
  }
  return [
    share,
    {
      verb: "copyToOrg",
      label: "Copy to another org…",
      disabledReason: facts.otherOrgCount === 0 ? "No other org on this Mac" : undefined,
    },
    {
      verb: "useInAllOrgs",
      label: "Use in all orgs",
      disabledReason: item.inertKind === null ? NOT_INERT_REASON : undefined,
    },
    remove,
  ];
}

/** Where the item lives, as a move/copy/remove context. */
export function scratchSource(item: ScratchItem): Context | null {
  if (item.where === "allOrgs") {
    return { id: allOrgsContextId(), kind: "personal", sharing: "personal", label: ALL_ORGS_LABEL };
  }
  if (item.orgSlug === null) return null;
  return {
    id: scratchSpaceContextId(item.orgSlug),
    kind: "personal",
    sharing: "personal",
    label: SCRATCH_SPACE_LABEL,
  };
}

/** A team of the item's org, as a Share target — labelled by the team's own name. */
export function teamTarget(orgSlug: string, teamName: string): Context {
  return {
    id: teamAllContextId(orgSlug, teamName),
    kind: "team-all",
    sharing: "shared",
    label: teamName,
    teamName,
  };
}

/** Another org's scratch space, as a Copy target. */
export function otherOrgTarget(orgSlug: string, orgName: string): Context {
  return {
    id: scratchSpaceContextId(orgSlug),
    kind: "personal",
    sharing: "personal",
    label: scratchSpaceInLabel(orgName),
  };
}

/** The all-orgs folder ("Use in all orgs") — or, for "Only this org", the org's own
 *  scratch space. */
export function allOrgsTarget(): Context {
  return { id: allOrgsContextId(), kind: "personal", sharing: "personal", label: ALL_ORGS_LABEL };
}

export function thisOrgTarget(orgSlug: string, orgName: string): Context {
  return otherOrgTarget(orgSlug, orgName);
}

/** The file name a row shows. */
export function itemFileName(logical: string): string {
  const slash = logical.lastIndexOf("/");
  return slash === -1 ? logical : logical.slice(slash + 1);
}
