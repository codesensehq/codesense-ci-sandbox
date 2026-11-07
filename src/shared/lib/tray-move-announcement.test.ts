// SPDX-License-Identifier: MIT
import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { clearTrayMove, recordTrayMove, useLastTrayMove } from "./tray-move-announcement";

const CORE = "acme:core";
const OPS = "acme:ops";

afterEach(() => {
  clearTrayMove(CORE);
  clearTrayMove(OPS);
});

describe("the last tray move, per team", () => {
  it("keeps each team's move: a move in another team's tray never replaces it", () => {
    const core = renderHook(() => useLastTrayMove(CORE));
    const ops = renderHook(() => useLastTrayMove(OPS));
    act(() => recordTrayMove(CORE, "w1", { before: "w2" }));
    act(() => recordTrayMove(OPS, "o1", { after: "o2" }));
    expect(core.result.current).toMatchObject({ teamKey: CORE, workspaceId: "w1" });
    expect(ops.result.current).toMatchObject({ teamKey: OPS, workspaceId: "o1" });
    // Clearing one team's move leaves the other's.
    act(() => clearTrayMove(OPS));
    expect(ops.result.current).toBeNull();
    expect(core.result.current).toMatchObject({ workspaceId: "w1" });
  });

  it("clears only the move it names: a newer one of the same team stays", () => {
    const core = renderHook(() => useLastTrayMove(CORE));
    act(() => recordTrayMove(CORE, "w1", { before: "w2" }));
    const first = core.result.current!.seq;
    act(() => recordTrayMove(CORE, "w1", { before: "w2" }));
    const second = core.result.current!.seq;
    expect(second).not.toBe(first);
    act(() => clearTrayMove(CORE, first));
    expect(core.result.current?.seq).toBe(second);
    act(() => clearTrayMove(CORE, second));
    expect(core.result.current).toBeNull();
  });

  it("follows the team the hook is asked about", () => {
    act(() => recordTrayMove(OPS, "o1", { after: "o2" }));
    const hook = renderHook(({ team }) => useLastTrayMove(team), { initialProps: { team: CORE } });
    expect(hook.result.current).toBeNull();
    hook.rerender({ team: OPS });
    expect(hook.result.current).toMatchObject({ workspaceId: "o1" });
  });
});
