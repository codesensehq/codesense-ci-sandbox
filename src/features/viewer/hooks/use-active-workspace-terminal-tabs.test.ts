// SPDX-License-Identifier: MIT
import { projectSessionStatus, resetSessionProjection } from "@/test/session-projection-fixture";
import { describe, it, expect, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { topMark } from "@/shared/lib/status-mark";
import type { SoftClosedWorkspace, Workspace } from "@/shared/stores/viewer-store";
import { buildRootWorkspaceId } from "@/shared/lib/team-key";
import { seedTerminalTabs, installTerminalLayoutHarness } from "@/test/terminal-layout-harness";
import {
  getActiveWorkspaceTabs,
  getActiveWorkspaceActiveTab,
  useScopeMark,
  useSessionProgress,
} from "./use-active-workspace-terminal-tabs";

installTerminalLayoutHarness();

// T.2: per-source-repo root id (replaces legacy ROOT_WORKSPACE_ID = "root").
// The `firstRootIdForTeam` fallback in the hook resolves to this id when the
// seeded workspace state has a single root with this id.
const ROOT_WORKSPACE_ID = buildRootWorkspaceId("test/repo");

const REPO = "/test/repo";
const WS_STORY = "story-30-10";

interface Seed {
  activeWorkspace?: Record<string, string>;
  softClosedWorkspaces?: Record<string, Record<string, SoftClosedWorkspace>>;
  /** Each workspace's tab session ids, in strip order (tab id === session id). */
  tabs?: Record<string, string[]>;
}

/** The default world: a root with one tab and a story with two (t3 focused). */
function seedStore(seed: Seed = {}) {
  useViewerStore.setState({
    // T.2: seed a root workspace under REPO so `firstRootIdForTeam`
    // resolves to ROOT_WORKSPACE_ID for the "active not set" fallback.
    workspaces: {
      [REPO]: [
        { id: ROOT_WORKSPACE_ID, kind: "root", cwd: "", status: "ready" },
        { id: WS_STORY, kind: "story", cwd: `/wt/${WS_STORY}`, status: "ready" },
      ],
    },
    activeWorkspace: seed.activeWorkspace ?? { [REPO]: ROOT_WORKSPACE_ID },
    softClosedWorkspaces: seed.softClosedWorkspaces ?? {},
  });
  const tabs = seed.tabs ?? { [ROOT_WORKSPACE_ID]: ["t1"], [WS_STORY]: ["t2", "t3"] };
  for (const [wsId, sessions] of Object.entries(tabs)) {
    seedTerminalTabs(REPO, wsId, sessions, {
      active: wsId === WS_STORY && sessions.includes("t3") ? "t3" : undefined,
    });
  }
}

beforeEach(() => {
  resetSessionProjection();
  useViewerStore.setState({
    ...useViewerStore.getInitialState(),
    terminalLayout: useViewerStore.getState().terminalLayout,
  });
});

describe("getActiveWorkspaceTabs", () => {
  it("returns tabs for the active workspace (root)", () => {
    seedStore();
    const tabs = getActiveWorkspaceTabs(REPO);
    expect(tabs).toHaveLength(1);
    expect(tabs[0].id).toBe("t1");
  });

  it("returns tabs for a story workspace when active", () => {
    seedStore({ activeWorkspace: { [REPO]: WS_STORY } });
    const tabs = getActiveWorkspaceTabs(REPO);
    expect(tabs).toHaveLength(2);
    expect(tabs[0].id).toBe("t2");
  });

  it("falls back to empty array when workspace has no tabs", () => {
    seedStore({ activeWorkspace: { [REPO]: "nonexistent" } });
    const tabs = getActiveWorkspaceTabs(REPO);
    expect(tabs).toEqual([]);
  });

  it("falls back to root when activeWorkspace is not set", () => {
    seedStore({ activeWorkspace: {} });
    const tabs = getActiveWorkspaceTabs(REPO);
    expect(tabs).toHaveLength(1);
    expect(tabs[0].id).toBe("t1");
  });

  it("returns empty array for unknown repo", () => {
    seedStore();
    expect(getActiveWorkspaceTabs("/unknown")).toEqual([]);
  });
});

describe("getActiveWorkspaceActiveTab", () => {
  it("returns the active tab ID for root workspace", () => {
    seedStore();
    expect(getActiveWorkspaceActiveTab(REPO)).toBe("t1");
  });

  it("returns the active tab ID for story workspace", () => {
    seedStore({ activeWorkspace: { [REPO]: WS_STORY } });
    expect(getActiveWorkspaceActiveTab(REPO)).toBe("t3");
  });

  it("returns undefined for unknown workspace", () => {
    seedStore({ activeWorkspace: { [REPO]: "nonexistent" } });
    expect(getActiveWorkspaceActiveTab(REPO)).toBeUndefined();
  });
});

describe("useScopeMark (SW-FX-52 scalar, minimized-aware)", () => {
  const minimized = (id: string): Workspace => ({
    id,
    kind: "story",
    cwd: `/wt/${id}`,
    status: "ready",
  });
  const ws = (...wsIds: string[]) => ({ teamKey: REPO, wsIds });

  beforeEach(() => {
    useAgentStateStore.setState({
      bySession: {},
      activityBySession: {},
      exitBySession: {},
      seenAt: {},
      stateSince: {},
    });
  });

  it("rolls a live workspace's tabs up to the most urgent mark and names them by state", () => {
    seedStore({ tabs: { [WS_STORY]: ["s2", "s3"] } });
    act(() => {
      projectSessionStatus("s2", "tool-running");
    });
    act(() => {
      projectSessionStatus("s3", "paused");
    });
    const { result } = renderHook(() => useScopeMark(ws(WS_STORY)));
    expect(result.current.mark).toBe("permission");
    expect(result.current.label).toBe("1 needs approval · 1 running a tool");
    expect(result.current.words).toBe("1 needs approval, 1 running a tool");
  });

  it("one member with a mark speaks for itself, with its own words", () => {
    seedStore({ tabs: { [WS_STORY]: ["s2", "s3"] } });
    act(() => {
      projectSessionStatus("s3", "paused");
    });
    const { result } = renderHook(() => useScopeMark(ws(WS_STORY)));
    expect(result.current.words).toBe("Needs approval");
  });

  it("an epic with a needs-you child and a working child shows needs you", () => {
    seedStore({ tabs: { [WS_STORY]: ["e1"], [ROOT_WORKSPACE_ID]: ["e2"] } });
    act(() => {
      projectSessionStatus("e1", "running");
    });
    act(() => {
      projectSessionStatus("e2", "paused");
    });
    const { result } = renderHook(() => useScopeMark(ws(WS_STORY, ROOT_WORKSPACE_ID)));
    expect(result.current.mark).toBe("permission");
  });

  it("an epic with a crashed child and a working child shows crashed, live as the child ends", () => {
    seedStore({ tabs: { [WS_STORY]: ["c1"], [ROOT_WORKSPACE_ID]: ["c2"] } });
    act(() => {
      projectSessionStatus("c1", "tool-running");
    });
    act(() => {
      projectSessionStatus("c2", "tool-running");
    });
    const { result } = renderHook(() => useScopeMark(ws(WS_STORY, ROOT_WORKSPACE_ID)));
    expect(result.current.mark).toBe("tool");
    act(() =>
      useAgentStateStore.setState({
        exitBySession: {
          c2: { code: null, signal: "SIGKILL", requested: false, outcome: "crashed", at: 1 },
        },
      }),
    );
    expect(result.current.mark).toBe("crashed");
    expect(result.current.words).toBe("1 crashed, 1 running a tool");
  });

  it("a finished turn reads idle, seen or not (below work in a roll-up)", () => {
    seedStore({ tabs: { [WS_STORY]: ["d1", "d2"] } });
    act(() => {
      projectSessionStatus("d1", "idle");
    });
    act(() => useAgentStateStore.setState({ stateSince: { d1: Date.now() } }));
    const { result } = renderHook(() => useScopeMark(ws(WS_STORY)));
    expect(result.current.mark).toBe("ready");
    act(() => useAgentStateStore.getState().markSeen("d1", Date.now() + 60_000));
    expect(result.current.mark).toBe("ready");
    act(() => {
      projectSessionStatus("d2", "tool-running");
    });
    expect(result.current.mark).toBe("tool");
  });

  it("returns a real mark for a MINIMIZED workspace (its tabs stay in the host)", () => {
    seedStore({
      softClosedWorkspaces: { [REPO]: { "30-99": { workspace: minimized("30-99") } } },
      tabs: { "30-99": ["sc1"] },
    });
    act(() => {
      projectSessionStatus("sc1", "tool-running");
    });
    const { result } = renderHook(() => useScopeMark(ws("30-99")));
    expect(result.current.mark).toBe("tool");
    act(() => {
      projectSessionStatus("sc1", "idle");
    });
    expect(result.current.mark).toBe("ready");
  });

  it("AC#2: a status change in workspace B does NOT re-render workspace A's mark", () => {
    seedStore({ tabs: { [WS_STORY]: ["sA"], [ROOT_WORKSPACE_ID]: ["sB"] } });
    let aRenders = 0;
    const { result } = renderHook(() => {
      aRenders++;
      return useScopeMark(ws(WS_STORY));
    });
    const rendersAfterMount = aRenders;
    expect(result.current.mark).toBeUndefined();
    act(() => {
      projectSessionStatus("sB", "running");
    });
    expect(aRenders).toBe(rendersAfterMount);
    act(() => {
      projectSessionStatus("sA", "tool-running");
    });
    expect(aRenders).toBe(rendersAfterMount + 1);
    expect(result.current.mark).toBe("tool");
  });

  it("the roll-up always equals the top of the per-session marks", () => {
    seedStore();
    const values = [undefined, "tool-running", "idle", "failed", "paused"] as const;
    const ids = ["d1", "d2", "d3"];
    const members = ids.map((id) => renderHook(() => useScopeMark({ sessionIds: [id] })));
    const aggregate = renderHook(() => useScopeMark({ sessionIds: ids }));
    for (const a of values)
      for (const b of values)
        for (const c of values) {
          const map: Record<string, (typeof values)[number]> = { d1: a, d2: b, d3: c };
          act(() => {
            resetSessionProjection();
            for (const [id, status] of Object.entries(map))
              if (status !== undefined) projectSessionStatus(id, status);
          });
          expect(aggregate.result.current.mark).toBe(
            topMark(members.map((member) => member.result.current.mark)),
          );
        }
    members.forEach((member) => member.unmount());
    aggregate.unmount();
  });

  it("SW-AM-4: useSessionProgress tracks one session's progress; a redundant write is a no-op", () => {
    useViewerStore.setState({ progressBySession: {} });
    const hook = renderHook(() => useSessionProgress("sP"));
    expect(hook.result.current).toBeUndefined();

    act(() => useViewerStore.getState().setSessionProgress("sP", { phase: "dev", note: "go" }));
    expect(hook.result.current).toEqual({ phase: "dev", note: "go" });

    // A field-equal re-write keeps slice identity stable (no-op-on-equal).
    const before = useViewerStore.getState().progressBySession;
    act(() => useViewerStore.getState().setSessionProgress("sP", { phase: "dev", note: "go" }));
    expect(useViewerStore.getState().progressBySession).toBe(before);

    // A different session's progress leaves this one untouched (scalar isolation).
    act(() => useViewerStore.getState().setSessionProgress("sOther", { phase: "review" }));
    expect(hook.result.current).toEqual({ phase: "dev", note: "go" });
  });

  it("AC#6: status fan-out is flat (O(1)) — flipping one of N=8 workspaces re-renders only that one", () => {
    const N = 8;
    const wsIds = Array.from({ length: N }, (_, i) => `ws-${i}`);
    seedStore({ tabs: Object.fromEntries(wsIds.map((id, i) => [id, [`s${i}`]])) });
    const renders = wsIds.map(() => 0);
    const hooks = wsIds.map((wsId, i) =>
      renderHook(() => {
        renders[i]++;
        return useScopeMark({ teamKey: REPO, wsIds: [wsId] });
      }),
    );
    const baseline = [...renders];

    // Flip exactly ONE session (workspace index 3).
    act(() => {
      projectSessionStatus("s3", "running");
    });

    // Only ws-3's hook re-rendered; the other 7 are untouched → the re-render
    // cost of a status tick is independent of N (flat, not O(N)).
    wsIds.forEach((_, i) => expect(renders[i]).toBe(baseline[i] + (i === 3 ? 1 : 0)));
    hooks.forEach((hk) => hk.unmount());
  });
});
