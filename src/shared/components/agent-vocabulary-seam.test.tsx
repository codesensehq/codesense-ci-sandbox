// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@/test-utils";

import { DECISION_VERBS, REACH_IT_VERB } from "@/shared/lib/agent-status-config";
import { AgentStatusChip } from "@/shared/components/agent-status-chip";
import type { AttentionRowDto } from "@/tauri/commands";

// SW-AV-7 — the cross-surface vocabulary SEAM. Activity (WeftAttentionRow) and Fleet
// (AgentStatusChip's nav button) must draw the ONE "reach it" verb from the SAME shared
// constant, so the go/Open/Steer synonym drift can never return (REVIEW-SYNTHESIS res. 17).
// This asserts the seam directly: both surfaces render the identical `REACH_IT_VERB`, the
// decision verbs stay distinct from it, and the status chip stays INERT (a badge, not a
// button) — the founder's prior "button-like status chip" trap.

const revealPanelById = vi.fn();
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: Object.assign(() => undefined, {
    subscribe: () => () => {},
    getState: () => ({
      revealPanelById,
      softClosedWorkspaces: {},
    }),
  }),
  findTabBySessionId: () => undefined,
}));

vi.mock("@/shared/lib/send-to-terminal", () => ({
  sendTextToSession: vi.fn(),
}));

// Imported AFTER the store mock so the row resolves the stubbed store.
import { WeftAttentionRow } from "@/features/weft/components/weft-attention-row";

function attentionRow(overrides: Partial<AttentionRowDto> = {}): AttentionRowDto {
  return {
    id: 1,
    summary: "Parked — waiting to reach the agent.",
    source: null,
    ref: null,
    initiator: "agent",
    ...overrides,
  };
}

describe("SW-AV-7 · one verb / chip / status vocabulary across Activity + Fleet", () => {
  it("Activity's reach-it primary verb IS the shared REACH_IT_VERB constant", () => {
    // A PARKED row is the pure "reach the agent / route where it resolves" case — the verb
    // that used to read the "Open" synonym.
    render(<WeftAttentionRow row={attentionRow({ kind: "parked" })} status="done" />);
    expect(screen.getByTestId("weft-row-primary-verb")).toHaveTextContent(REACH_IT_VERB);
  });

  it("Fleet's status-chip navigation control names the SAME shared REACH_IT_VERB", () => {
    render(<AgentStatusChip status="running" label="SW-A" onClick={() => {}} />);
    // The Fleet nav chip's accessible name is "{REACH_IT_VERB} to {agent}, {status}".
    const btn = screen.getByRole("button");
    expect(btn.getAttribute("aria-label")).toMatch(new RegExp(`^${REACH_IT_VERB} to SW-A,`));
  });

  it("the reach-it verb is NOT a decision verb (distinct verbs for distinct decisions)", () => {
    expect(Object.values(DECISION_VERBS)).not.toContain(REACH_IT_VERB as string);
    // The distinct decisions each keep their own word.
    expect(DECISION_VERBS.ratify).toBe("Ratify");
    expect(DECISION_VERBS.approve).toBe("Approve");
    expect(DECISION_VERBS.answer).toBe("Answer");
    expect(DECISION_VERBS.ack).toBe("Ack");
  });

  it("a status chip renders INERT (a role=img badge, never a button) when it is not a nav control", () => {
    render(<AgentStatusChip status="running" />);
    expect(screen.getByRole("img", { name: /status:/i })).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });
});
