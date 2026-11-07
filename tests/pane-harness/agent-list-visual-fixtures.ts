// SPDX-License-Identifier: MIT
// Preview world for the live agent-list pane (SV-AGP-9). Not a test.
import { readFileSync } from "node:fs";
const faces = JSON.parse(
  readFileSync("src/assets/avatar-packs/builtin-funny/pack.json", "utf8"),
).faces.map((f: any) => f.id);
type A = {
  name: string;
  started?: string;
  noName?: boolean;
  facts?: boolean;
  role: string;
  status: string;
  ws: string;
  team: string;
  parent?: string;
  model?: string;
  ask?: string;
  subs?: number;
  offline?: boolean;
  machine?: string;
  pinned?: boolean;
  mins?: number;
  note?: string;
};
const T1 = "codesense:riverside",
  T2 = "codesense:core";
const AG: A[] = [
  {
    name: "Sprint 1 manager",
    facts: true,
    role: "Manager",
    status: "running",
    ws: "main",
    team: T1,
    mins: 340,
  },
  {
    name: "SV-AGP-9 lead",
    facts: true,
    role: "Lead",
    status: "running",
    ws: "SV-AGP-9",
    team: T1,
    parent: "Sprint 1 manager",
    subs: 2,
    mins: 124,
  },
  {
    name: "Design worker",
    role: "Designer",
    status: "thinking",
    ws: "SV-AGP-9",
    team: T1,
    parent: "SV-AGP-9 lead",
    mins: 41,
    note: "Capturing direction B in Nord",
  },
  {
    name: "Codex dev",
    started:
      "Focus areas:\n1. **Owned-review** of the pane\n2. **MCP deadlines**\n- `pnpm test` only\n- **Cost:** no new watchers",
    facts: true,
    role: "Developer",
    status: "idle",
    ws: "SV-AGP-9",
    team: T1,
    parent: "SV-AGP-9 lead",
    model: "codex",
    subs: 2,
    mins: 18,
  },
  {
    name: "Reviewer",
    subs: 1,
    role: "Reviewer",
    status: "paused",
    ws: "SV-AGP-9",
    team: T1,
    parent: "Codex dev",
    mins: 6,
  },
  {
    name: "SV-INP-13 lead",
    role: "Lead",
    status: "idle",
    ws: "SV-INP-13",
    team: T1,
    parent: "Sprint 1 manager",
    mins: 180,
  },
  {
    name: "Inbox build",
    started:
      "Focus areas:\n1. **Owned-review** of the pane\n2. **MCP deadlines**\n- `pnpm test` only\n- **Cost:** no new watchers",
    noName: true,
    role: "Developer",
    status: "tool-running",
    ws: "SV-INP-13",
    team: T1,
    parent: "SV-INP-13 lead",
    model: "codex",
    mins: 52,
  },
  {
    name: "Contrast check",
    subs: 1,
    role: "QA",
    status: "completed",
    ws: "SV-INP-13",
    team: T1,
    parent: "SV-INP-13 lead",
  },
  {
    name: "Small fixes",
    role: "Developer",
    status: "awaiting-answer",
    ws: "main",
    team: T1,
    parent: "Sprint 1 manager",
    ask: "Merge #101 now,\n- or **wait** for the wave-end run?\n2. reply here",
    mins: 60,
    pinned: true,
  },
  {
    name: "Release notes",
    role: "Writer",
    status: "idle",
    ws: "main",
    team: T1,
    parent: "Sprint 1 manager",
    mins: 25,
  },
  { name: "Docs writer", subs: 2, role: "Writer", status: "crashed", ws: "SV-DOC-5", team: T1 },
  {
    name: "Merge waiter",
    role: "Developer",
    status: "blocked",
    ws: "SV-DOC-5",
    team: T1,
    note: "Waiting on #102 to merge",
    mins: 30,
  },
  { name: "Lint bot", role: "QA", status: "errored", ws: "WV-12", team: T2, mins: 12 },
  {
    name: "Docs reviewer",
    role: "Reviewer",
    status: "stalled",
    ws: "SV-DOC-5",
    team: T1,
    subs: 1,
    mins: 9,
  },
  {
    name: "Perf soak",
    subs: 3,
    role: "QA",
    status: "running",
    ws: "main",
    team: T1,
    offline: true,
    machine: "build-2",
    mins: 120,
  },
  {
    name: "Sam's helper",
    role: "Assistant",
    status: "idle",
    ws: "",
    team: T1,
    mins: 8,
    pinned: true,
  },
  { name: "Core lead", role: "Lead", status: "running", ws: "WV-12", team: T2, mins: 90 },
  {
    name: "Core runner with a remarkably long agent name for the dock",
    started:
      "Focus areas:\n1. **Owned-review** of the pane\n2. **MCP deadlines**\n- `pnpm test` only\n- **Cost:** no new watchers",
    role: "Runner",
    status: "thinking",
    ws: "WV-12",
    team: T2,
    parent: "Core lead",
    model: "codex",
    mins: 14,
  },
  {
    name: "Planner",
    role: "Planner",
    status: "idle",
    ws: "WV-12",
    team: T2,
    parent: "Core lead",
    mins: 31,
  },
  { name: "Scheduler", role: "Developer", status: "running", ws: "WV-14", team: T2, mins: 22 },
  { name: "Inbox relay", role: "Developer", status: "running", ws: "WV-14", team: T2, mins: 40 },
  {
    name: "Builder",
    role: "Builder",
    status: "running",
    ws: "remote-build",
    team: T2,
    offline: true,
    machine: "build-1",
    mins: 240,
  },
];
const WS: Record<string, any> = {
  "SV-AGP-9": {
    kind: "story",
    storyStatus: "in-progress",
    stageRole: "active",
    stageLabel: "In progress",
  },
  "SV-INP-13": { kind: "story", storyStatus: "review", stageRole: "review", stageLabel: "Review" },
  "SV-DOC-5": {
    kind: "story",
    storyStatus: "in-progress",
    stageRole: "active",
    stageLabel: "In progress",
  },
  main: { kind: "repo" },
  "WV-12": {
    kind: "story",
    storyStatus: "in-progress",
    stageRole: "active",
    stageLabel: "In progress",
  },
  "WV-14": { kind: "story", storyStatus: "todo", stageRole: "not_started", stageLabel: "To do" },
  "remote-build": { kind: "repo", machineId: "build-1" },
};
const handle = (n: string) => n.toLowerCase().replace(/[^a-z0-9]+/g, "-");
export function previewWorld(opts: { arrange?: string; selected?: string; count?: number } = {}) {
  const now = Date.now();
  const list = AG.slice(0, opts.count ?? AG.length);
  const sessions = list.map((a, i) => ({
    id: "s" + i,
    sessionId: "s" + i,
    handle: handle(a.name),
    kind: "cli",
    alive: a.status !== "completed",
    state: a.offline ? "offline" : "running",
    effectiveStatus: a.status,
    needsYouKind:
      a.status === "awaiting-answer" ? "question" : a.status === "paused" ? "permission" : null,
    pendingAsk: a.ask || null,
    team: { org: a.team.split(":")[0], team: a.team.split(":")[1] },
    machine: a.machine || "This Mac",
    machineId: a.machine || "local",
    model: a.model || "opus",
    ...(a.noName
      ? {}
      : { rowNames: { primary: a.name, secondary: a.role, full: a.name }, displayName: a.name }),
    startedWith: a.started || null,
    searchText: `${a.name} ${a.role} ${a.ws}`,
    subagents: a.subs || 0,
    ...(a.facts
      ? {
          face: {
            kind: "image",
            ref: `builtin-funny:${faces[i % faces.length]}`,
            src: `/kit/v1/avatars/builtin-funny/${faces[i % faces.length]}.svg`,
          },
          faceTag: 10 + i,
          rolePiece: {
            piece: i ? "worker" : "manager",
            label: "Manager",
            src: "/kit/v1/avatars/builtin-chess/" + (i ? "pawn" : "queen") + ".svg",
          },
        }
      : i % 3 === 0
        ? {}
        : { avatar: `builtin-funny:${faces[i % faces.length]}` }),
    launchedBy: a.parent ? handle(a.parent) : null,
    startedAtMs: a.mins ? now - a.mins * 60000 : null,
    progress: a.note ? { note: a.note } : null,
    pinned: !!a.pinned,
    activityAtMs: a.status === "stalled" ? now - 11 * 60000 : null,
  }));
  const idOf = (n?: string) => (n ? "r" + list.findIndex((a) => a.name === n) : null);
  const wsOrder: Record<string, string[]> = {};
  for (const a of list)
    if (a.ws && !(wsOrder[a.team] ??= []).includes(a.ws)) wsOrder[a.team].push(a.ws);
  const refs = list.map((a, i) => ({
    id: "r" + i,
    sessionRef: "s" + i,
    inboxRef: "counts:team:" + a.team.replace(":", "/"),
    teamKey: a.team,
    parentId: idOf(a.parent),
    parentSource: a.parent ? "launcher" : null,
    order: i,
    workspaceRef: a.ws || null,
    workspaceRank: a.ws ? wsOrder[a.team].indexOf(a.ws) : 99,
    workspace: a.ws
      ? { id: a.ws, label: a.ws, sourceRepoSlug: "alderco/codesense", ...WS[a.ws] }
      : null,
    crewParentId:
      a.parent && list.find((p) => p.name === a.parent)?.ws === a.ws ? idOf(a.parent) : null,
    crewOrder: i,
    crewLead: !a.parent || list.find((p) => p.name === a.parent)?.ws !== a.ws,
  }));
  const teams = [T1, T2].filter((t) => list.some((a) => a.team === t));
  return {
    title: "Agents",
    slotId: "dock" as const,
    width: 236,
    height: 1500,
    sessions,
    inbox: teams.map((t) => ({
      id: "counts:team:" + t.replace(":", "/"),
      rowType: "counts",
      team: { org: "codesense", team: t.split(":")[1] },
      humanUnread: 0,
      recipients: {},
      sessions: {},
      needsYouBySender: {},
    })),
    topics: {
      agentList: [
        ...refs,
        {
          id: "agentList:state",
          rowType: "state",
          order: Number.MAX_VALUE,
          ready: true,
          pane: {
            workspaceOrder: wsOrder,
            pinnedRefs: list.map((a, i) => (a.pinned ? "r" + i : null)).filter(Boolean),
          },
          homeTeam: T1,
          teamOrder: teams,
          teamNames: Object.fromEntries(
            teams.map((t) => [t, { org: "codesense", team: t.split(":")[1] }]),
          ),
          workspaceRollups: {},
        },
      ],
      "ui:dock": [
        {
          id: "ui:dock",
          arrange: opts.arrange || "teams",
          folds: { "#ws-fold:codesense:core|WV-14": true },
          selected: opts.selected ?? "s2",
          pins: {},
        },
      ],
    },
  };
}
