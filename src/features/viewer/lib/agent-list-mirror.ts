// SPDX-License-Identifier: MIT
import { railInfo } from "./group-workspaces-by-epic";
import { customRailKey } from "./workspace-groups";
import type { ViewerState } from "@/shared/stores/viewer-store";
import { buildTeamKey, NO_TEAM_SENTINEL, parseTeamKey } from "@/shared/lib/team-key";
import type { Org } from "@/shared/types/domain";
import { teamWorkspaceOrder } from "./workspace-order";

type Inputs = Pick<
  ViewerState,
  | "openedTeams"
  | "workspaces"
  | "softClosedWorkspaces"
  | "workspaceOrderByTeam"
  | "trayBaseOrderByTeam"
  | "railKeyByWorkspaceId"
  | "customGroupsByTeam"
>;

/** These remain workspace-owned inputs; the hub applies their shared order to agent groups. */
export function agentListMirrorInputs(
  state: Inputs,
  orgs: readonly Pick<Org, "slug" | "display_name" | "teams">[] = [],
) {
  const teams = new Set([
    ...state.openedTeams,
    ...Object.keys(state.workspaces),
    ...Object.keys(state.softClosedWorkspaces),
  ]);
  const names = new Map(orgs.map((org) => [org.slug, org.display_name]));
  const knownTeams = new Set([
    ...teams,
    ...orgs.flatMap((org) => org.teams.map((team) => buildTeamKey(org.slug, team.name))),
  ]);
  // Collation follows the viewer's locale; Rust consumes the resulting ranks unchanged.
  const orderedTeams = [...knownTeams]
    .filter((key) => key !== NO_TEAM_SENTINEL)
    .sort((a, b) => {
      const left = parseTeamKey(a);
      const right = parseTeamKey(b);
      const orgOrder =
        (names.get(left.orgSlug) || left.orgSlug)
          .toLowerCase()
          .localeCompare((names.get(right.orgSlug) || right.orgSlug).toLowerCase()) ||
        left.orgSlug.localeCompare(right.orgSlug);
      if (orgOrder !== 0) return orgOrder;
      return (
        (left.teamName ?? left.orgSlug)
          .toLowerCase()
          .localeCompare((right.teamName ?? right.orgSlug).toLowerCase()) ||
        (a < b ? -1 : a > b ? 1 : 0)
      );
    });
  return {
    agentPane: {
      openTeams: state.openedTeams,
      teamOrder: orderedTeams,
      rails: Object.fromEntries(
        [...teams].map((team) => [
          team,
          [
            ...new Set([
              ...Object.values(state.railKeyByWorkspaceId[team] ?? {}),
              ...(state.customGroupsByTeam?.[team] ?? []).map((g) => customRailKey(g.id)),
            ]),
          ].map((key) => ({
            railKey: key,
            ...railInfo(team, key, state.customGroupsByTeam?.[team]),
          })),
        ]),
      ),
      railKeys: state.railKeyByWorkspaceId,
    },
    agentTeamNames: Object.fromEntries(
      orderedTeams.map((key) => {
        const { orgSlug, teamName } = parseTeamKey(key);
        return [key, { org: names.get(orgSlug) || orgSlug, team: teamName ?? orgSlug }];
      }),
    ),
    openedTeams: state.openedTeams,
    workspaces: state.workspaces,
    softClosedWorkspaces: state.softClosedWorkspaces,
    agentTeamRanks: Object.fromEntries(orderedTeams.map((team, rank) => [team, rank])),
    resolvedWorkspaceOrderByTeam: Object.fromEntries(
      [...teams].map((team) => [
        team,
        teamWorkspaceOrder({
          base: state.trayBaseOrderByTeam[team],
          workspaces: state.workspaces[team],
          softClosed: state.softClosedWorkspaces[team],
          stored: state.workspaceOrderByTeam[team],
          groups: state.railKeyByWorkspaceId[team],
        }),
      ]),
    ),
  };
}
