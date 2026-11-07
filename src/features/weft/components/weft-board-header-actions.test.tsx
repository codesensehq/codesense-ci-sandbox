// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@/test-utils";

import type { AttentionBoardDto } from "@/tauri/commands";

// SLW-TL-4 — the header is a single PASSIVE, neutral "N need you" count. No Dispatch,
// no jump, no governance controls.

let board: AttentionBoardDto;
vi.mock("../hooks/use-attention-board", () => ({
  useAttentionBoard: () => ({ board, degraded: null, error: null, reload: () => {} }),
}));
let sessions: unknown[] = [];
vi.mock("../hooks/use-sessions-needing-you", () => ({
  useSessionsNeedingYou: () => sessions,
}));
vi.mock("@/shared/lib/team-key", () => ({
  useActiveTeamKey: () => "org:team",
  NO_TEAM_SENTINEL: "__no_team__",
}));

import { WeftBoardHeaderActions } from "./weft-board-header-actions";

const EMPTY: AttentionBoardDto = {
  youOwe: [],
  owedToYou: [],
  onAgent: [],
  parked: [],
  doneUnacked: [],
};

beforeEach(() => {
  sessions = [];
  board = {
    ...EMPTY,
    youOwe: [{ id: 1, summary: "x", source: null, ref: null, initiator: "human" }],
  };
});

describe("WeftBoardHeaderActions", () => {
  it("shows a neutral 'N need you' count, never the alarm-red status color", () => {
    render(<WeftBoardHeaderActions />);
    const count = screen.getByTestId("inbox-needs-you-count");
    expect(count).toHaveTextContent("1 need you");
    expect(count.className).toContain("text-muted-foreground");
    expect(count.className).not.toMatch(/red|destructive/i);
  });

  it("renders nothing when nothing needs you", () => {
    board = EMPTY;
    render(<WeftBoardHeaderActions />);
    expect(screen.queryByTestId("inbox-needs-you-count")).toBeNull();
  });

  it("carries no Dispatch / governance control", () => {
    render(<WeftBoardHeaderActions />);
    expect(screen.queryByTestId("weft-board-dispatch-trigger")).toBeNull();
    expect(screen.queryByText(/dispatch/i)).toBeNull();
    expect(screen.queryByTestId("weft-posture-control")).toBeNull();
  });
});
