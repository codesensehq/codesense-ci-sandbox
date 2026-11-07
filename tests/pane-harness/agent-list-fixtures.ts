// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import type { PaneFixtures } from "./harness";
export { AGENT_LIST_PANE } from "../../scripts/lib/builtin-panes.mjs";
export function agentListFixtures(count: number, arrange = "manager"): PaneFixtures {
  const sessions = Array.from({ length: count }, (_, i) => ({
    id: `s${i}`,
    sessionId: `s${i}`,
    handle: `agent-${i}`,
    searchText: `Agent ${i} Developer agent-${i} Work ${Math.floor(i / 10)} dev`,
    kind: "cli",
    alive: true,
    state: "running",
    effectiveStatus: i === 2 ? "awaiting-answer" : "running",
    needsYouKind: i === 2 ? "question" : null,
    pendingAsk: i === 2 ? "Which branch should I use?" : null,
    team: { org: "fixture", team: "dev" },
    machine: "This Mac",
    machineId: "local",
    model: "opus",
    rowNames: { primary: `Agent ${i}`, secondary: "Developer", full: `Agent ${i}` },
    subagents: i === 1 ? 2 : 0,
    launchedBy: i === 1 ? "agent-0" : i === 2 ? "agent-1" : null,
  }));
  const refs = sessions.map((s, i) => ({
    id: `r${i}`,
    sessionRef: s.id,
    inboxRef: "counts:team:fixture/dev",
    teamKey: "fixture:dev",
    parentId: i === 1 ? "r0" : i === 2 ? "r1" : null,
    parentSource: i === 1 || i === 2 ? "launcher" : null,
    order: i,
    workspaceRef: `ws${Math.floor(i / 10)}`,
    workspaceRank: Math.floor(i / 10),
    workspace: {
      id: `ws${Math.floor(i / 10)}`,
      label: `Work ${Math.floor(i / 10)}`,
      kind: "story",
      storyStatus: "in-progress",
      stageRole: "active",
      stageLabel: "In progress",
      sourceRepoSlug: "fixture/repo",
    },
    crewParentId: i === 1 ? "r0" : i === 2 ? "r1" : null,
    crewOrder: i,
    crewLead: i % 10 === 0,
  }));
  return {
    title: "Agents",
    slotId: "dock",
    width: 268,
    height: 480,
    sessions,
    inbox: [
      {
        id: "counts:team:fixture/dev",
        rowType: "counts",
        team: { org: "fixture", team: "dev" },
        humanUnread: 0,
        recipients: {},
        sessions: {},
        needsYouBySender: {},
      },
    ],
    topics: {
      agentList: [
        ...refs,
        {
          id: "agentList:state",
          rowType: "state",
          order: Number.MAX_VALUE,
          ready: true,
          pane: {
            workspaceOrder: {
              "fixture:dev": Array.from({ length: Math.ceil(count / 10) }, (_, i) => `ws${i}`),
            },
          },
          homeTeam: "fixture:dev",
          teamOrder: ["fixture:dev"],
          teamNames: { "fixture:dev": { org: "Fixture org", team: "dev" } },
          workspaceRollups: {
            "fixture:dev": Object.fromEntries(
              Array.from({ length: Math.ceil(count / 10) }, (_, i) => [
                `ws${i}`,
                {
                  mark: i === 0 && count > 2 ? "question" : "thinking",
                  count: i === 0 && count > 2 ? 1 : Math.min(10, count - i * 10),
                  total: Math.min(10, count - i * 10),
                  summary:
                    i === 0 && count > 2
                      ? `1 needs you, ${Math.min(count, 10) - 1} thinking`
                      : `${Math.min(10, count - i * 10)} thinking`,
                },
              ]),
            ),
          },
        },
      ],
      "ui:dock": [{ id: "ui:dock", arrange, folds: {}, selected: null, pins: {} }],
    },
  };
}

/** The same explicitly named identities and neutral activity inputs for both capture paths. */
export function agentListCaptureFixtures(): PaneFixtures {
  const fixtures = agentListFixtures(8, "teams");
  const faces = JSON.parse(readFileSync("src/assets/avatar-packs/builtin-funny/pack.json", "utf8"))
    .faces as { id: string }[];
  fixtures.sessions!.forEach((session, index) => {
    session.avatar = `builtin-funny:${faces[index].id}`;
    session.displayName = (session.rowNames as { primary: string }).primary;
    session.subagents = 0;
    session.pendingAsk = null;
    delete session.model;
  });
  const state = fixtures.topics!.agentList.find((row) => row.rowType === "state")!;
  state.teamNames = { "fixture:dev": { org: "fixture", team: "dev" } };
  return fixtures;
}
