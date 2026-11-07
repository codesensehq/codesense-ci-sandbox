// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";

import type { TerminalTabIndex } from "@/shared/stores/viewer-store";
import type { AgentRow, AgentSessionRow } from "@/tauri/commands";

import { agentTabRetitles, followAgentTabTitles, type AgentTabTitleDeps } from "./agent-tab-titles";

const TEAM = "acme:backend";
const row = (key: string, name: string, scope: AgentRow["scope"] = "team") =>
  ({ key, name, scope, org: scope === "team" ? "acme" : "builtin", team: "backend" }) as AgentRow;
const CONTROLLER = row("acme/backend/controller", "Controller");
const session = (sessionId: string, agentRevision: string) =>
  ({ sessionId, handle: `h-${sessionId}`, agentRevision }) as AgentSessionRow;
const tab = (id: string, name: string, remote = false) => ({
  id,
  name,
  sessionId: id,
  ...(remote ? { origin: { machineId: "box", member: "m", handle: "rh" } } : {}),
});

const SESSIONS = [
  session("ctl", "acme/backend/controller@1"),
  session("rev", "acme/backend/reviewer@2"),
  session("remote-rev", "acme/backend/reviewer@2"),
];
const INDEX = {
  [TEAM]: {
    content: [tab("ctl", "Controller")],
    story: [tab("rev", "Reviewer"), tab("remote-rev", "Reviewer", true), tab("mine", "My shell")],
  },
} as unknown as TerminalTabIndex;

describe("agent tab titles", () => {
  it("treats a saved agent named controller like any other: renamed, its tab follows", () => {
    const before = [CONTROLLER, row("acme/backend/reviewer", "Reviewer")];
    const after = [row("acme/backend/controller", "Ricky"), before[1]];
    expect(agentTabRetitles(INDEX, SESSIONS, before, after)).toEqual([
      { teamKey: TEAM, workspaceId: "content", tabId: "ctl", name: "Ricky" },
    ]);
  });

  it("saving a new agent named controller retitles no tab that runs another agent", () => {
    const before = [row("acme/backend/reviewer", "Reviewer")];
    const after = [row("acme/backend/controller", "Ricky"), ...before];
    expect(agentTabRetitles(INDEX, SESSIONS, before, after)).toEqual([]);
  });

  it("retitle a renamed agent's tabs, local and remote, leaving tabs someone titled", () => {
    const before = [CONTROLLER, row("acme/backend/reviewer", "Reviewer")];
    const after = [CONTROLLER, row("acme/backend/reviewer", "Critic")];
    const index = {
      [TEAM]: { ...INDEX[TEAM], other: [tab("rev2", "My review")] },
    } as unknown as TerminalTabIndex;
    const sessions = [...SESSIONS, session("rev2", "acme/backend/reviewer@2")];
    expect(agentTabRetitles(index, sessions, before, after)).toEqual([
      { teamKey: TEAM, workspaceId: "story", tabId: "rev", name: "Critic" },
      { teamKey: TEAM, workspaceId: "story", tabId: "remote-rev", name: "Critic" },
    ]);
  });

  it("follows a rename that arrives while no agents list is mounted, in minimized tabs too", () => {
    let push: (rows: AgentRow[], before: AgentRow[] | null) => void = () => {};
    const rename = vi.fn();
    const minimized = {
      [TEAM]: { tray: [tab("tray-rev", "Reviewer")] },
    } as unknown as TerminalTabIndex;
    const deps: AgentTabTitleDeps = {
      watch: () => () => {},
      onRows: (onRows) => {
        push = onRows;
        return () => {};
      },
      tabIndexes: () => [INDEX, minimized],
      sessions: () => [...SESSIONS, session("tray-rev", "acme/backend/reviewer@2")],
      rename,
    };
    const stop = followAgentTabTitles(deps);
    const first = [CONTROLLER, row("acme/backend/reviewer", "Reviewer")];
    push(first, null);
    expect(rename).not.toHaveBeenCalled();
    push([row("acme/backend/controller", "Ricky"), row("acme/backend/reviewer", "Critic")], first);
    expect(rename.mock.calls.map(([t]) => [t.workspaceId, t.tabId, t.name])).toEqual([
      ["content", "ctl", "Ricky"],
      ["story", "rev", "Critic"],
      ["story", "remote-rev", "Critic"],
      ["tray", "tray-rev", "Critic"],
    ]);
    stop();
  });
});
