// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import type { RoleQueue } from "@/tauri/commands";

import { createMockBackend } from "../mock-backend";
import { resolveScenario } from "../scenario";
import {
  agentTeamActivitySnapshots,
  agentTeamInboxHandlers,
  agentTeamsOrgHandlers,
  agentTeamSessions,
  SECOND_ORG_SLUG,
  SECOND_TEAM_KEY,
  SECOND_TEAM_STATUSES,
} from "./agents-team";

type Row = { id: number; to: string | null; toRole: string | null; unread: boolean };

function handlers() {
  return agentTeamInboxHandlers(() => {}) as unknown as Record<
    string,
    (args: Record<string, unknown>) => unknown
  >;
}
const rollup = (h: ReturnType<typeof handlers>) =>
  (h.list_inbox_rollup!({}) as { messages: Row[] }).messages;

describe("agent team inbox mock: parity with the real human send and status", () => {
  it("supplies the chosen team's complete role directory through the assembled mock backend", async () => {
    const invoke = createMockBackend(resolveScenario(""), "?agents=team");
    const roles = (await invoke("agent_read_roles")) as { roles: RoleQueue[] };
    expect(roles.roles.map((r) => r.role)).toContain("reviewer");
    expect(roles.roles.every((r) => Array.isArray(r.registrations))).toBe(true);
    expect(roles.roles.find((r) => r.role === "triage")?.counts).toEqual({
      pending: 1,
      claimed: 1,
      accepted: 1,
      acknowledged: 1,
    });
    expect(await invoke("agent_read_roles", { team: { org: "elsewhere", team: "empty" } })).toEqual(
      {
        roles: [],
      },
    );
  });

  it("refuses a blank recipient and a role named human", () => {
    const h = handlers();
    for (const to of ["", "@", "@  "])
      expect(h.send_inbox_as_human!({ to, text: "hi" })).toMatchObject({ status: "invalid" });
    expect(h.send_inbox_as_human!({ to: "#human", text: "hi" })).toMatchObject({
      status: "invalid",
    });
  });

  it("reads the message a send answers", () => {
    const h = handlers();
    const unread = rollup(h).find((m) => m.to === "human" && m.unread);
    expect(unread).toBeDefined();
    const sent = h.send_inbox_as_human!({ to: "#triage", text: "on it", inReplyTo: unread!.id });
    expect(sent).toMatchObject({ status: "sent" });
    expect(rollup(h).find((m) => m.id === unread!.id)?.unread).toBe(false);
  });

  it("reads #role:<id> and role:<id> as the role <id>, as the real send does", () => {
    const h = handlers();
    for (const to of ["#role:triage", "role:triage"]) {
      const sent = h.send_inbox_as_human!({ to, text: `via ${to}` }) as { messageId: number };
      const row = rollup(h).find((m) => m.id === sent.messageId);
      expect(row).toMatchObject({ toRole: "triage" });
    }
  });

  it("refuses an unknown presence status, as the real enum does", () => {
    const h = handlers();
    expect(() => h.set_human_status!({ status: "napping" })).toThrow();
    expect(h.set_human_status!({ status: "busy", note: "call" })).toMatchObject({
      status: "busy",
      note: "call",
    });
  });
});

describe("the ?agents=teams world: a second open team in a second org", () => {
  it("adds the second team's agents only in that world", () => {
    const team = agentTeamSessions("?agents=team");
    const teams = agentTeamSessions("?agents=teams");
    expect(teams.length - team.length).toBe(5);
    expect(team.some((r) => r.handle.startsWith("SV-3#"))).toBe(false);
    const second = teams.filter((r) => r.sessionId?.startsWith("sess-sv-"));
    expect(second.every((r) => r.alive && r.status === "running")).toBe(true);
    // Hooks say which are mid-turn (Working) and which finished their turn (Ready).
    const hooks = SECOND_TEAM_STATUSES.map((s) => s.status);
    expect(hooks).toContain("running");
    expect(hooks).toContain("done");
    expect(new Set(SECOND_TEAM_STATUSES.map((s) => s.tab_session_id))).toEqual(
      new Set(second.map((r) => r.sessionId)),
    );
  });

  it("discovers and loads the second org, and opens its team with its workspaces", async () => {
    const invoke = createMockBackend(resolveScenario(""), "?agents=teams");
    const orgs = (await invoke("discover_local_orgs")) as { slug: string; display_name: string }[];
    expect(orgs.find((o) => o.slug === SECOND_ORG_SLUG)?.display_name).toBe("Abacus Labs");
    expect(orgs.length).toBeGreaterThanOrEqual(3);
    const loaded = (await invoke("load_org_cmd", { slug: SECOND_ORG_SLUG })) as { slug: string };
    expect(loaded.slug).toBe(SECOND_ORG_SLUG);
    const state = JSON.parse((await invoke("load_app_state")) as string) as {
      viewer: { openedTeams: string[]; workspaces: Record<string, { id: string }[]> };
    };
    expect(state.viewer.openedTeams).toContain(SECOND_TEAM_KEY);
    expect(state.viewer.workspaces[SECOND_TEAM_KEY]?.map((w) => w.id)).toContain("SV-3");
  });

  it("opens the second team with its own active workspace and order, under the one shown CLI and split", async () => {
    const invoke = createMockBackend(resolveScenario(""), "?agents=teams");
    const { viewer } = JSON.parse((await invoke("load_app_state")) as string) as {
      viewer: {
        workspaces: Record<string, { id: string }[]>;
        activeWorkspace: Record<string, string>;
        cliChrome: { visible: boolean; splitRatio: number };
        workspaceOrderByTeam: Record<string, string[]>;
      };
    };
    const ids = viewer.workspaces[SECOND_TEAM_KEY].map((w) => w.id);
    expect(ids).toContain(viewer.activeWorkspace[SECOND_TEAM_KEY]);
    expect(viewer.cliChrome.visible).toBe(true);
    expect(viewer.cliChrome.splitRatio).toBeTypeOf("number");
    expect(viewer.workspaceOrderByTeam[SECOND_TEAM_KEY]).toEqual(ids);
  });

  it("draws the second team with its own avatar pack and the first with the built-in ones", async () => {
    const invoke = createMockBackend(resolveScenario(""), "?agents=teams");
    const first = (await invoke("avatar_packs_list_for_team", {
      teamKey: "acme/widgets:default",
    })) as { packs: unknown[]; default_pack: string | null };
    const second = (await invoke("avatar_packs_list_for_team", { teamKey: SECOND_TEAM_KEY })) as {
      packs: { id: string; faces: { ref: string; src: string }[] }[];
      default_pack: string | null;
    };
    expect(first).toMatchObject({ packs: [], default_pack: null });
    expect(second.default_pack).toBe(second.packs[0]?.id);
    expect(second.packs[0]?.faces.length).toBeGreaterThan(1);
    expect(second.packs[0]?.faces[0]?.src).toMatch(/^data:image\/svg\+xml;base64,/);
  });

  it("lists the second org once, even when the org it wraps already discovers it", async () => {
    const already = { slug: SECOND_ORG_SLUG, display_name: "Abacus Labs" };
    const h = agentTeamsOrgHandlers({
      discover_local_orgs: async () => [{ slug: "acme" }, already],
    });
    const orgs = (await h.discover_local_orgs!({})) as { slug: string }[];
    expect(orgs.map((o) => o.slug)).toEqual(["acme", SECOND_ORG_SLUG]);
  });

  it("leaves the ?agents=team world with one org of its own", async () => {
    const invoke = createMockBackend(resolveScenario(""), "?agents=team");
    const orgs = (await invoke("discover_local_orgs")) as { slug: string }[];
    expect(orgs.some((o) => o.slug === SECOND_ORG_SLUG)).toBe(false);
  });
});

describe("the team worlds' subagents", () => {
  it("report running subagents through the host's activity snapshot, per world", () => {
    const one = agentTeamActivitySnapshots("?agents=team");
    expect(one.map((a) => [a.session, a.subagentsOpen])).toEqual([["sess-login-lead", 2]]);
    const two = agentTeamActivitySnapshots("?agents=teams");
    expect(two.map((a) => [a.session, a.state, a.subagentsOpen])).toEqual([
      ["sess-login-lead", "working", 2],
      ["sess-sv-lead", "working", 3],
      ["sess-sv-review", "idle", 1],
    ]);
    // Each is a session the world runs, observed, so its agent is never Ready while they run.
    const sessions = new Set(agentTeamSessions("?agents=teams").map((r) => r.sessionId));
    for (const a of two) {
      expect(sessions.has(a.session)).toBe(true);
      expect(a.observed).toBe(true);
    }
  });
});
