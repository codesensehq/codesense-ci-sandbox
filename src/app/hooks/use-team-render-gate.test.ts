// SPDX-License-Identifier: MIT
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { useTeamRenderGate } from "./use-team-render-gate";

const EMPTY = new Set<string>();
const KEYS = ["o:a", "o:b", "o:c", "o:d", "p:e"];

describe("useTeamRenderGate (warm scopes, no unmount timer)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    act(() => {
      for (const k of KEYS) warmScopes.forget(k);
    });
  });

  const gate = (opened: string[], dirty: ReadonlySet<string> = EMPTY) =>
    renderHook((props) => useTeamRenderGate(props), {
      initialProps: { openedTeams: opened, dirtyTeamKeys: dirty },
    });
  const activate = (k: string | null) => act(() => warmScopes.activate(k));

  it("mounts only the active team on first render", () => {
    activate("o:a");
    const { result } = gate(["o:a", "o:b"]);
    expect([...result.current]).toEqual(["o:a"]);
  });

  it("keeps the outgoing team mounted however long it stays away", () => {
    activate("o:a");
    const { result } = gate(["o:a", "o:b"]);
    activate("o:b");
    expect(result.current.has("o:a")).toBe(true);
    act(() => void vi.advanceTimersByTime(60_000));
    expect(result.current.has("o:a")).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("A→B→A performs no unmount and arms no timer", () => {
    activate("o:a");
    const { result } = gate(["o:a", "o:b"]);
    activate("o:b");
    activate("o:a");
    expect([...result.current].sort()).toEqual(["o:a", "o:b"]);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("keeps three teams mounted across orgs and suspends the fourth", () => {
    for (const k of ["o:a", "o:b", "p:e"]) activate(k);
    const { result } = gate(["o:a", "o:b", "o:c", "p:e"]);
    expect([...result.current].sort()).toEqual(["o:a", "o:b", "p:e"]);
    activate("o:c");
    expect(result.current.has("o:a")).toBe(false);
    expect([...result.current].sort()).toEqual(["o:b", "o:c", "p:e"]);
  });

  it("keeps a dirty team mounted while it has unsaved edits, then lets it go", () => {
    activate("o:a");
    const dirty = new Set(["o:a"]);
    const { result, rerender } = gate(["o:a", "o:b", "o:c", "o:d"], dirty);
    for (const k of ["o:b", "o:c", "o:d"]) activate(k);
    expect(result.current.has("o:a")).toBe(true);
    rerender({ openedTeams: ["o:a", "o:b", "o:c", "o:d"], dirtyTeamKeys: EMPTY });
    expect(result.current.has("o:a")).toBe(false);
  });

  it("an aged dirty team stays pinned while another team's dirty state changes", () => {
    const gone = vi.fn();
    warmScopes.setGoneHandler(gone);
    const opened = ["o:a", "o:b", "o:c", "o:d", "p:e"];
    activate("o:a");
    const { result, rerender } = gate(opened, new Set(["o:a"]));
    for (const k of ["o:b", "o:c", "o:d", "p:e", "o:b", "o:c", "o:d"]) activate(k);
    expect(warmScopes.tierOf("o:a")).toBe("warm");
    rerender({ openedTeams: opened, dirtyTeamKeys: new Set(["o:a", "o:d"]) });
    rerender({ openedTeams: opened, dirtyTeamKeys: new Set(["o:a"]) });
    expect(gone).not.toHaveBeenCalled();
    expect(result.current.has("o:a")).toBe(true);
    rerender({ openedTeams: opened, dirtyTeamKeys: EMPTY });
    expect(gone).toHaveBeenCalledWith("o:a");
    warmScopes.setGoneHandler(null);
  });

  it("a rename of a dirty team leaves it pinned until its edits are saved", () => {
    const gone = vi.fn();
    warmScopes.setGoneHandler(gone);
    activate("o:a");
    const { rerender } = gate(["o:a", "o:b", "o:c", "o:d", "p:e"], new Set(["o:a"]));
    act(() => warmScopes.rekey("o:a", "o:a2"));
    const opened = ["o:a2", "o:b", "o:c", "o:d", "p:e"];
    rerender({ openedTeams: opened, dirtyTeamKeys: new Set(["o:a2"]) });
    for (const k of ["o:b", "o:c", "o:d", "p:e", "o:b", "o:c", "o:d"]) activate(k);
    expect(warmScopes.tierOf("o:a2")).toBe("warm");
    rerender({ openedTeams: opened, dirtyTeamKeys: EMPTY });
    expect(warmScopes.tierOf("o:a2")).toBe("gone");
    expect(gone).toHaveBeenCalledWith("o:a2");
    warmScopes.setGoneHandler(null);
    act(() => warmScopes.forget("o:a2"));
  });

  it("drops a closed team at once", () => {
    activate("o:a");
    const { result, rerender } = gate(["o:a", "o:b"]);
    activate("o:b");
    expect(result.current.has("o:a")).toBe(true);
    rerender({ openedTeams: ["o:b"], dirtyTeamKeys: EMPTY });
    expect(result.current.has("o:a")).toBe(false);
  });
});
