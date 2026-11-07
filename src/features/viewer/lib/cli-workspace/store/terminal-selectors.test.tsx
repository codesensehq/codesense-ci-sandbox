// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import {
  useTerminalVisible,
  useWorkspaceTabsSlice,
  useActiveForegroundTileCount,
  useFlatForegroundLayout,
  useRailKeyIndex,
  useSoftClosedForTeam,
} from "./terminal-selectors";

const TEAM = "/test/repo";
const WS = "ws-1";

installTerminalLayoutHarness();

function seed(overrides: Record<string, unknown> = {}) {
  useViewerStore.setState({
    terminalVisible: false,
    aiStatusBySession: {},
    progressBySession: {},
    ...overrides,
  } as never);
  seedTerminalTabs(TEAM, WS, ["s1"]);
}

/**
 * AC4 (no-render-storm): the atomic CLI selector hooks must NOT re-render on a
 * store tick unrelated to their slice. This is the observable, runnable half of
 * AC4 — the instance-identity / PTY-not-respawned half lives in the e2e-real
 * harness (deferred execution). A regression here (e.g. an object-returning
 * selector without `useShallow`, or a coarsened selector) shows up as an extra
 * render on an unrelated tick.
 */
describe("OC-9b terminal-selectors — atomic subscriptions bail out on unrelated ticks (AC4)", () => {
  beforeEach(() => seed());

  it("useTerminalVisible does not re-render when an unrelated session's status ticks", () => {
    let renders = 0;
    const { result } = renderHook(() => {
      renders++;
      return useTerminalVisible();
    });
    expect(result.current).toBe(false);
    const baseline = renders;

    // Unrelated tick: a session AI-status change touches a different slice.
    act(() => {
      useViewerStore.setState({ aiStatusBySession: { s1: "running" } } as never);
    });
    expect(renders).toBe(baseline); // Object.is bail-out — no re-render

    // Positive control: changing the selected slice DOES re-render, once.
    act(() => {
      useViewerStore.setState({ terminalVisible: true } as never);
    });
    expect(renders).toBe(baseline + 1);
    expect(result.current).toBe(true);
  });

  it("useWorkspaceTabsSlice keeps a stable reference across an unrelated tick", () => {
    const { result } = renderHook(() => useWorkspaceTabsSlice(TEAM, WS));
    const first = result.current;
    expect(first.map((t) => t.sessionId)).toEqual(["s1"]);
    act(() => {
      useViewerStore.setState({ progressBySession: { s1: { fraction: 0.5 } } } as never);
    });
    // Same projected array reference → no fresh-object churn into consumers.
    expect(result.current).toBe(first);
  });

  it("useActiveForegroundTileCount returns a primitive that is stable across an unrelated tick", () => {
    let renders = 0;
    const { result } = renderHook(() => {
      renders++;
      return useActiveForegroundTileCount(TEAM);
    });
    const baseline = renders;
    const count = result.current;
    act(() => {
      useViewerStore.setState({ progressBySession: { s1: { fraction: 0.9 } } } as never);
    });
    expect(renders).toBe(baseline);
    expect(result.current).toBe(count);
  });

  it("stored-reference object hooks keep identity across an unrelated tick (no fresh-object churn)", () => {
    useViewerStore.setState({
      foregroundLayout: { [TEAM]: { preset: "columns" } },
      railKeyByWorkspaceId: { [TEAM]: { [WS]: "rail-1" } },
      softClosedWorkspaces: { [TEAM]: {} },
    } as never);
    const flat = renderHook(() => useFlatForegroundLayout(TEAM));
    const rail = renderHook(() => useRailKeyIndex(TEAM));
    const soft = renderHook(() => useSoftClosedForTeam(TEAM));
    const flat0 = flat.result.current;
    const rail0 = rail.result.current;
    const soft0 = soft.result.current;

    act(() => {
      useViewerStore.setState({ progressBySession: { s1: { fraction: 0.3 } } } as never);
    });

    // Each returns the SAME stored reference — a fresh object/array here (a
    // `?? {}` or `.map()` regression) would re-render the terminal grid.
    expect(flat.result.current).toBe(flat0);
    expect(rail.result.current).toBe(rail0);
    expect(soft.result.current).toBe(soft0);
  });
});
