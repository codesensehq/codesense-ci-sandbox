// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { AgentRow } from "@/tauri/commands";

import { agentKeyFor, agentRefFor, findAgent, offeredTo, routineSpace } from "./routine-agent-ref";

const row = (key: string, scope: "personal" | "team", org: string, team: string) =>
  ({ key, scope, org, team }) as AgentRow;

const TEAM = row("acme/api/backend/triage", "team", "acme/api", "backend");
const OTHER = row("acme/api/frontend/triage", "team", "acme/api", "frontend");
const MINE = row("_personal/1/personal/dev", "personal", "_personal", "personal");

describe("routine agent references", () => {
  const team = routineSpace("team", "acme/api/backend/standup");
  const personal = routineSpace("personal", "_personal/1/personal/morning");

  it("writes a bare id in the routine's own space and a key elsewhere", () => {
    expect(agentRefFor(TEAM, team)).toBe("triage");
    expect(agentRefFor(MINE, personal)).toBe("dev");
    expect(agentRefFor(TEAM, personal)).toBe("acme/api/backend/triage");
  });

  it("finds a bare id only in the routine's own space", () => {
    const rows = [OTHER, TEAM, MINE];
    expect(findAgent(rows, "triage", team)).toBe(TEAM);
    expect(findAgent(rows, "triage", personal)).toBeUndefined();
    expect(findAgent(rows, "acme/api/frontend/triage", personal)).toBe(OTHER);
    expect(findAgent(rows, " ", personal)).toBeUndefined();
  });

  it("offers a team routine only its team, a personal routine yours and the team in view", () => {
    expect([TEAM, OTHER, MINE].map((r) => offeredTo(r, team, "acme/api/frontend"))).toEqual([
      true,
      false,
      false,
    ]);
    expect([TEAM, OTHER, MINE].map((r) => offeredTo(r, personal, "acme/api/backend"))).toEqual([
      true,
      false,
      true,
    ]);
    expect(offeredTo(TEAM, personal, null)).toBe(false);
  });

  it("renders the key a routine's reference names", () => {
    expect(agentKeyFor("triage", "acme/api/backend/standup")).toBe("acme/api/backend/triage");
    expect(agentKeyFor("dev", "_personal/1/personal/morning")).toBe("_personal/1/personal/dev");
    expect(agentKeyFor("acme/api/backend/triage", "_personal/1/personal/morning")).toBe(
      "acme/api/backend/triage",
    );
  });
});
