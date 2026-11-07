// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { AgentRow } from "@/tauri/commands";

import { findAgent } from "./agent-names";

const row = (key: string, scope: AgentRow["scope"], org = "", team = ""): AgentRow =>
  ({ key, name: key, scope, org, team }) as AgentRow;

const ROWS = [
  row("acme/backend/triage", "team", "acme", "backend"),
  row("acme/frontend/triage", "team", "acme", "frontend"),
  row("_personal/1/personal/triage", "personal"),
  row("_personal/1/personal/dev", "personal"),
  row("builtin/claude", "builtin", "builtin"),
  row("builtin/codex", "builtin", "builtin"),
];

describe("findAgent", () => {
  it("finds a key as is", () => {
    expect(findAgent("acme/frontend/triage", ROWS)?.key).toBe("acme/frontend/triage");
    expect(findAgent("acme/ops/triage", ROWS)).toBeNull();
  });

  it("finds a bare id in the team first, then in your personal agents", () => {
    expect(findAgent("triage", ROWS, "acme:backend")?.key).toBe("acme/backend/triage");
    expect(findAgent("triage", ROWS, "acme:ops")?.key).toBe("_personal/1/personal/triage");
    expect(findAgent("dev", ROWS, "acme:backend")?.key).toBe("_personal/1/personal/dev");
    expect(findAgent("ghost", ROWS, "acme:backend")).toBeNull();
    expect(findAgent("  ", ROWS)).toBeNull();
  });

  it("finds a starter by key, or by bare id when no team or personal agent has it", () => {
    expect(findAgent("builtin/codex", ROWS)?.key).toBe("builtin/codex");
    expect(findAgent("claude", ROWS, "acme:backend")?.key).toBe("builtin/claude");
    // A trigger's owner may have a personal `codex`, which you can't see.
    expect(findAgent("codex", ROWS, "acme:backend", { personal: false })).toBeNull();
    const mine = [row("_personal/1/personal/claude", "personal"), ...ROWS];
    expect(findAgent("claude", mine, "acme:backend")?.key).toBe("_personal/1/personal/claude");
  });

  it("with personal off, a bare id resolves only in the team", () => {
    expect(findAgent("triage", ROWS, "acme:backend", { personal: false })?.key).toBe(
      "acme/backend/triage",
    );
    expect(findAgent("dev", ROWS, "acme:backend", { personal: false })).toBeNull();
  });
});

describe("findAgent and archived agents", () => {
  const archived = { ...row("acme/backend/claude", "team", "acme", "backend"), archived: true };
  const starter = row("builtin/claude", "builtin", "builtin");
  it("skips an archived agent for a bare id, as a launch does, but names it by key", () => {
    expect(findAgent("claude", [archived, starter], "acme:backend")?.key).toBe("builtin/claude");
    expect(findAgent("acme/backend/claude", [archived, starter])).toBe(archived);
  });
});
