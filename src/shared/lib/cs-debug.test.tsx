// SPDX-License-Identifier: MIT
import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { csDebug, csDebugEnabled, csMark } from "./cs-debug";
import { markSwitchRevealed, startSwitchProfile, useSwitchCommitMark } from "./switch-profile";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org } from "@/shared/types/domain";

beforeEach(() => {
  delete window.__csDebug;
  performance.clearMarks();
  useOrgStore.setState({
    orgs: [{ slug: "acme", teams: [{ name: "a" }, { name: "b" }] } as unknown as Org],
    activeOrgSlug: "acme",
    activeTeamName: "a",
  });
});
afterEach(() => {
  vi.unstubAllEnvs();
  vi.useRealTimers();
});

describe("the debug gate", () => {
  it("is off by default: no buffer, no marks, no console output", () => {
    const debug = vi.spyOn(console, "debug").mockImplementation(() => {});
    const mark = vi.spyOn(performance, "mark");
    expect(csDebugEnabled()).toBe(false);
    csDebug("x", { a: 1 });
    csMark("y");
    expect(window.__csDebug).toBeUndefined();
    expect(mark).not.toHaveBeenCalled();
    expect(debug).not.toHaveBeenCalled();
    debug.mockRestore();
  });

  it("starts no switch profile and marks nothing while off", () => {
    const stop = startSwitchProfile();
    useOrgStore.getState().setActiveTeam("b");
    markSwitchRevealed();
    stop();
    expect(window.__csDebug).toBeUndefined();
    function Probe() {
      useSwitchCommitMark("acme:b");
      return <p>child</p>;
    }
    render(<Probe />);
    expect(screen.getByText("child")).toBeTruthy();
    expect(window.__csDebug).toBeUndefined();
  });

  it("records into a ring buffer capped at 500 when on", () => {
    vi.stubEnv("MODE", "webdriver");
    vi.spyOn(console, "debug").mockImplementation(() => {});
    for (let i = 0; i < 520; i++) csDebug("n", { i });
    expect(window.__csDebug).toHaveLength(500);
    expect(window.__csDebug![0].i).toBe(20);
  });
});

describe("a team switch profile (gate on)", () => {
  it("records the write, the commit, the settled effects and the reveal as one `switch` entry", async () => {
    vi.stubEnv("MODE", "webdriver");
    vi.spyOn(console, "debug").mockImplementation(() => {});
    vi.useFakeTimers({
      toFake: ["setTimeout", "clearTimeout", "requestAnimationFrame", "performance"],
    });
    const mark = vi.spyOn(performance, "mark");
    function Probe() {
      const team = useOrgStore((s) => s.activeTeamName);
      useSwitchCommitMark(team);
      return <p>{team}</p>;
    }
    const stop = startSwitchProfile();
    render(<Probe />);
    await act(async () => {
      useOrgStore.getState().setActiveTeam("b");
    });
    markSwitchRevealed();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(300);
    });
    stop();
    const summary = window.__csDebug!.find((e) => e.kind === "switch");
    expect(summary).toMatchObject({ to: "acme:b" });
    expect(summary!.writeToFirstCommitMs).not.toBeNull();
    expect(summary!.writeToSettledMs).not.toBeNull();
    expect(summary!.writeToRevealedMs).not.toBeNull();
    expect(mark.mock.calls.map(([name]) => name)).toEqual([
      "cs:switch:store-write",
      "cs:switch:first-commit",
      "cs:switch:effects-settled",
      "cs:switch:right-pane-revealed",
    ]);
  });

  it("ends a switch no reveal follows on the fallback, with the commit still recorded", async () => {
    vi.stubEnv("MODE", "webdriver");
    vi.spyOn(console, "debug").mockImplementation(() => {});
    vi.useFakeTimers({
      toFake: ["setTimeout", "clearTimeout", "requestAnimationFrame", "performance"],
    });
    function Probe() {
      const team = useOrgStore((s) => s.activeTeamName);
      useSwitchCommitMark(team);
      return <p>{team}</p>;
    }
    const stop = startSwitchProfile();
    render(<Probe />);
    await act(async () => {
      useOrgStore.getState().setActiveTeam("b");
      await vi.advanceTimersByTimeAsync(1100);
    });
    stop();
    const summary = window.__csDebug!.find((e) => e.kind === "switch");
    expect(summary).toMatchObject({ writeToRevealedMs: null });
    expect(summary!.writeToFirstCommitMs).not.toBeNull();
  });
});
