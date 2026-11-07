// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { agentListMirrorInputs } from "./agent-list-mirror";

const inputs = () => ({
  openedTeams: ["zoo:dev", "eclair:dev"],
  workspaces: {},
  softClosedWorkspaces: {},
  workspaceOrderByTeam: {},
  trayBaseOrderByTeam: {},
  railKeyByWorkspaceId: {},
  customGroupsByTeam: {},
});
const org = (slug: string, display_name: string, teams: string[]) => ({
  slug,
  display_name,
  teams: teams.map((name) => ({ name, source_repos: [] })),
});

describe("agent-list team ranks", () => {
  it("keeps locale order for accented organisation and team names", () => {
    const mirrored = agentListMirrorInputs(inputs(), [
      org("zoo", "Zoo", ["dev"]),
      org("eclair", "Éclair", ["Zulu", "Équipe", "dev"]),
    ]);
    expect(mirrored.agentTeamRanks).toEqual({
      "eclair:dev": 0,
      "eclair:Équipe": 1,
      "eclair:Zulu": 2,
      "zoo:dev": 3,
    });
    expect(mirrored.openedTeams).toEqual(["zoo:dev", "eclair:dev"]);
  });

  it("ranks registry, opened, open-workspace and minimized-workspace teams together", () => {
    const mirrored = agentListMirrorInputs(
      {
        ...inputs(),
        openedTeams: ["opened:dev", "_no_team:_none"],
        workspaces: { "open:dev": [] },
        softClosedWorkspaces: { "minimized:dev": {} },
      },
      [org("registry", "Registry", ["other"])],
    );
    expect(mirrored.agentTeamRanks).toEqual({
      "minimized:dev": 0,
      "open:dev": 1,
      "opened:dev": 2,
      "registry:other": 3,
    });
  });

  it("uses locale-aware organisation slugs to break equal display names", () => {
    const mirrored = agentListMirrorInputs({ ...inputs(), openedTeams: [] }, [
      org("zoo", "Shared", ["dev"]),
      org("éclair", "Shared", ["dev"]),
    ]);
    expect(mirrored.agentTeamRanks).toEqual({ "éclair:dev": 0, "zoo:dev": 1 });
  });
});

it("mirrors team display names for host-owned heads", () => {
  expect(
    agentListMirrorInputs(inputs(), [org("eclair", "Éclair", ["dev"])]).agentTeamNames,
  ).toMatchObject({
    "eclair:dev": { org: "Éclair", team: "dev" },
    "zoo:dev": { org: "zoo", team: "dev" },
  });
});

it("mirrors workspace group labels and membership independently of pane scope", () => {
  const state = inputs();
  const result = agentListMirrorInputs({
    ...state,
    railKeyByWorkspaceId: { "zoo:dev": { ws: " group:focus" } },
    customGroupsByTeam: { "zoo:dev": [{ id: "focus", name: "Focus", workspaceIds: ["ws"] }] },
  });
  expect(result.agentPane.openTeams).toEqual(state.openedTeams);
  expect(result.agentPane.railKeys).toEqual({ "zoo:dev": { ws: " group:focus" } });
  expect(result.agentPane.rails["zoo:dev"]).toEqual(
    expect.arrayContaining([expect.objectContaining({ title: "Focus", kind: "custom" })]),
  );
});
