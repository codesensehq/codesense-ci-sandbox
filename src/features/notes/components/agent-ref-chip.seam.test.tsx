// SPDX-License-Identifier: MIT
import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, renderHook, screen } from "@/test-utils";

import { useAgentStatusOf } from "@/features/viewer/hooks/use-agent-status-of";
import type { TeamAgentIdentity } from "@/features/viewer/hooks/use-team-agent-identities";
import { AGENT_STATUS_WORDS } from "@/shared/lib/agent-status-config";
import { markOfAgentStatus } from "@/shared/lib/status-mark";
import type { AgentSessionRow } from "@/tauri/commands";

import { NoteAgentsProvider } from "../lib/note-agents";
import { AgentRefChip } from "./agent-ref-chip";

// A handed-off line's chip reads the agent's status through the ONE shared pipe the
// agents list reads (`useAgentStatusOf` → `markOfAgentStatus` / `AGENT_STATUS_WORDS`),
// so the chip and the list can never disagree. Only the roster is stubbed.

const TEAM = "acme:platform";

const h = vi.hoisted(() => ({
  agents: [] as TeamAgentIdentity[],
  showAgent: vi.fn(() => Promise.resolve({ kind: "shown" })),
}));

vi.mock("@/features/viewer/hooks/use-team-agent-identities", () => ({
  useTeamAgentIdentities: () => h.agents,
}));
vi.mock("@/features/viewer/lib/show-agent", () => ({ showAgent: h.showAgent }));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn() }) }));

function row(handle: string, patch: Partial<AgentSessionRow> = {}): AgentSessionRow {
  return {
    handle,
    sessionId: `sess-${handle}`,
    kind: "dashboard-launched",
    alive: true,
    status: "running",
    effectiveStatus: "thinking",
    backgrounded: false,
    startedWith: null,
    startedAtMs: Date.now() - 60_000,
    launcher: "virtual://agents",
    agentRevision: null,
    lastOutputMs: null,
    ...patch,
  };
}

function identity(r: AgentSessionRow, label: string): TeamAgentIdentity {
  return {
    row: r,
    depth: 0,
    label,
    face: { kind: "mark", text: label.slice(0, 1), color: null },
    role: { piece: "worker", label: "Worker" },
    agentKey: null,
  };
}

beforeEach(() => {
  h.showAgent.mockClear();
});

describe("AgentRefChip — the handed-off line's live status", () => {
  it.each([
    ["running", row("BUG-1#dev")],
    ["paused (needs approval)", row("BUG-1#qa", { status: "paused", effectiveStatus: "paused" })],
    ["ended", row("BUG-1#done", { alive: false, status: "done", effectiveStatus: "done" })],
  ])("shows the shared mark and words for a %s agent", (_label, r) => {
    h.agents = [identity(r, "Jimmy")];
    const { result } = renderHook(() => useAgentStatusOf([r]));
    const status = result.current(r);

    render(
      <NoteAgentsProvider teamKey={TEAM}>
        <AgentRefChip handle={r.handle} name="Old name" />
      </NoteAgentsProvider>,
    );
    const chip = screen.getByRole("button", { name: /Jimmy/ });
    expect(chip).toHaveAttribute("data-mark", markOfAgentStatus(status));
    expect(chip).toHaveTextContent(AGENT_STATUS_WORDS[status].toLowerCase());
    expect(chip.querySelector(`.cs-mark-${markOfAgentStatus(status)}`)).not.toBeNull();
  });

  it("a finished turn reads Ready, never a done check", () => {
    const r = row("BUG-1#dev", { effectiveStatus: "idle" });
    h.agents = [identity(r, "Jimmy")];
    render(
      <NoteAgentsProvider teamKey={TEAM}>
        <AgentRefChip handle={r.handle} name="Jimmy" />
      </NoteAgentsProvider>,
    );
    const chip = screen.getByRole("button", { name: /Jimmy/ });
    // the status's shared mark is `ready` (hollow ring); the chip
    // uses the shared "Ready" vocabulary, lowercased as its label.
    expect(chip).toHaveAttribute("data-mark", "ready");
    expect(chip).toHaveTextContent("ready");
  });

  it("clicking it shows that agent", () => {
    const r = row("BUG-1#dev");
    h.agents = [identity(r, "Jimmy")];
    render(
      <NoteAgentsProvider teamKey={TEAM}>
        <AgentRefChip handle={r.handle} name="Jimmy" />
      </NoteAgentsProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: /Jimmy/ }));
    expect(h.showAgent).toHaveBeenCalledWith(r, { teamKey: TEAM });
  });

  it("an agent no longer running keeps its name and reads not running", () => {
    h.agents = [];
    render(
      <NoteAgentsProvider teamKey={TEAM}>
        <AgentRefChip handle="gone#1" name="Jimmy" />
      </NoteAgentsProvider>,
    );
    const chip = screen.getByRole("button", { name: /Jimmy/ });
    expect(chip).toHaveTextContent("not running");
    expect(chip).toHaveAttribute("data-mark", "ended");
    fireEvent.click(chip);
    expect(h.showAgent).not.toHaveBeenCalled();
  });
});
