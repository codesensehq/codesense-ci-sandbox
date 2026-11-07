// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import type { AttentionBoardDto, AttentionRowDto } from "@/tauri/commands";

import { countNeedsYou, isBoardEmpty, LANE_FALLBACK_LABEL, NEED_LABEL } from "./inbox-model";

function row(id: number, extra: Partial<AttentionRowDto> = {}): AttentionRowDto {
  return {
    id,
    summary: `row ${id}`,
    source: null,
    ref: null,
    initiator: "human",
    ...extra,
  };
}

function board(slices: Partial<AttentionBoardDto> = {}): AttentionBoardDto {
  return {
    youOwe: [],
    owedToYou: [],
    onAgent: [],
    parked: [],
    doneUnacked: [],
    ...slices,
  };
}

describe("countNeedsYou", () => {
  it("counts youOwe + owedToYou only (not running / done)", () => {
    expect(
      countNeedsYou(
        board({
          youOwe: [row(1), row(2)],
          owedToYou: [row(3)],
          onAgent: [row(4)],
          doneUnacked: [row(5)],
        }),
      ),
    ).toBe(3);
  });
});

describe("isBoardEmpty", () => {
  it("is true only when every slice is empty", () => {
    expect(isBoardEmpty(board())).toBe(true);
    expect(isBoardEmpty(board({ doneUnacked: [row(1)] }))).toBe(false);
  });
});

describe("vocabulary", () => {
  it("every need has a plain, non-control-plane label", () => {
    const forbidden = /owe|endorse|ratif|t2|subclass|dispatch|lease/i;
    for (const label of Object.values(NEED_LABEL)) {
      expect(label).not.toMatch(forbidden);
    }
    for (const label of Object.values(LANE_FALLBACK_LABEL)) {
      expect(label).not.toMatch(forbidden);
    }
  });
});
