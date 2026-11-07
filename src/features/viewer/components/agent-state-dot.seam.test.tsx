// SPDX-License-Identifier: MIT
import { projectSessionStatus, resetSessionProjection } from "@/test/session-projection-fixture";
/**
 * The sessions projection supplies status; real terminal layouts, seen flags and
 * tile/tray components verify the same mark, words and motion on every surface.
 * Process events remain here to exercise retained exit details and precedence.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, render, screen, within } from "@/test-utils";

const h = vi.hoisted(() => ({
  handlers: new Map<string, Set<(e: { payload: unknown }) => void>>(),
}));
// Like Tauri, every listener of an event hears it: the status listener is not the
// only `pty-exit` subscriber once the surfaces mount.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    let set = h.handlers.get(name);
    if (!set) h.handlers.set(name, (set = new Set()));
    set.add(cb);
    return () => set.delete(cb);
  }),
}));
// ActivityV1 recovery has its own seam tests; these surfaces consume the projection.
vi.mock("@/shared/hooks/use-terminal-activity", () => ({ useTerminalActivity: () => {} }));
vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  killPty: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  sessionsHaveLiveAgent: vi.fn(async (queries: { ptyId: string }[]) =>
    queries.map((q) => ({ ptyId: q.ptyId, alive: true })),
  ),
  listAgentSignals: vi.fn().mockResolvedValue([]),
}));
vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import { renderHook } from "@testing-library/react";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import { TerminalTileChrome } from "./terminal-tile-chrome";
import { WorkspaceTray } from "./workspace-tray";
import { markOfTerminalStatus, type StatusMarkKind } from "@/shared/lib/status-mark";
import { formatMarkLabel } from "@/shared/lib/terminal-dot-words";
import { useTerminalStatusEvents } from "@/shared/hooks/use-terminal-status-events";
import { resetLivenessPollerForTests } from "@/shared/hooks/use-agent-liveness-monitor";
import {
  resetDotSinceForTests,
  useScopeMark,
  useSessionAiStatus,
  watchDotSince,
} from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import {
  isSessionSeen,
  markSessionSeen,
  resetPrunedAgentSessionsForTests,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import {
  useViewerStore,
  type TerminalAiStatus,
  type TerminalTab,
  type Workspace,
} from "@/shared/stores/viewer-store";

// ── layout: happy-dom has no layout, so widths and chip rects are declared ──

const tileWidth: Record<string, number> = {};
class FakeResizeObserver {
  constructor(private cb: ResizeObserverCallback) {}
  observe(el: Element) {
    const id = el.getAttribute("data-testid")?.match(/^tile-chrome-(.+)$/)?.[1];
    const width = id ? (tileWidth[id] ?? 0) : 0;
    this.cb([{ contentRect: { width } } as ResizeObserverEntry], this as unknown as ResizeObserver);
  }
  unobserve() {}
  disconnect() {}
}

const TEAM = "acme:default";
const S = "sess-agent";
const PLAIN = "sess-plain";
const tab = (id: string, sessionId: string): TerminalTab => ({ id, name: id, sessionId });
const ws = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });

/** The agent session S sits in every surface at once: a visible pill (tile
 *  "pill"), a tile's overflow (tile "over"), a too-narrow tile's roll-up (tile
 *  "roll"), an in-view tray chip (w-a) and an out-of-view one (w-e). */
function renderSurfaces() {
  const noop = vi.fn();
  const chrome = (id: string, width: number, tabs: TerminalTab[]) => {
    tileWidth[id] = width;
    return (
      <TerminalTileChrome
        key={id}
        workspace={ws(id)}
        isFocused
        tabs={tabs}
        activeTabId={tabs[0]!.id}
        onCloseTab={noop}
        onHideTab={noop}
        onEndTab={noop}
        onEndOtherTabs={noop}
        onEndAllTabs={noop}
        onReorderTab={noop}
        onRenameTab={noop}
        onFocusTile={noop}
        onNewShell={noop}
        onToggleZoom={noop}
        onHideTile={noop}
        onCloseTile={noop}
        onSelectTab={noop}
      />
    );
  };
  render(
    <>
      {chrome("pill", 300, [tab("p1", S), tab("p2", "sess-other")])}
      {chrome("over", 300, [tab("o1", "sess-other"), tab("o2", "sess-x"), tab("o3", S)])}
      {chrome("roll", 80, [tab("r1", S)])}
      <WorkspaceTray teamKey={TEAM} tree={null} isLightTheme={false} onNewWorkspace={noop} />
    </>,
  );
}

/** Every surface's dot for session S, by where it sits. */
function surfaces(): Record<string, HTMLElement | null> {
  const rollup = screen.queryByTestId("tile-rollup-roll");
  return {
    "tab pill": screen.queryByTestId("tab-p1-ai-status"),
    'tile "+N"': screen.queryByTestId("tile-overflow-status-over"),
    "tile roll-up": rollup?.querySelector<HTMLElement>("[data-mark]") ?? null,
    "tray chip": screen.queryByTestId("workspace-tab-w-a-ai-status-dot"),
    'tray "+N"': screen.queryByTestId("workspace-tray-overflow-status"),
  };
}

/** Whether `el` draws the shared mark for `status`. */
const paints = (el: HTMLElement, status: TerminalAiStatus) =>
  el.getAttribute("data-mark") === markOfTerminalStatus(status);
const escape = (w: string) => w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

function expectEverySurface(
  status: TerminalAiStatus,
  mark: StatusMarkKind = markOfTerminalStatus(status),
) {
  for (const [where, el] of Object.entries(surfaces())) {
    expect(el, `${where} shows a mark`).not.toBeNull();
    expect(el!, `${where} draws ${mark}`).toHaveAttribute("data-mark", mark);
  }
  const label = escape(formatMarkLabel({ mark, status }));
  const words = escape(formatMarkLabel({ mark, status }, ", "));
  expect(screen.getByTestId("tile-rollup-roll").querySelector("[data-mark]")).toHaveAccessibleName(
    new RegExp(`^${label}( · |$)`),
  );
  expect(screen.getByTestId("tile-overflow-over")).toHaveAccessibleName(
    new RegExp(`more tabs?: ${words}(,|$)`),
  );
  // …and in words everywhere else it shows: the tab named by its name then its
  // state, the tray chip named with it, the tray's "+N" naming it.
  expect(
    within(screen.getByTestId("tile-chrome-pill")).getByRole("tab", { name: /^p1\b/ }),
  ).toHaveAccessibleName(new RegExp(`^p1 ${words}(,|$)`));
  expect(
    screen
      .getByTestId("workspace-tab-w-a-ai-status-dot")
      .closest("[data-tray-chip]")!
      .querySelector("[data-ws-row]"),
  ).toHaveAccessibleName(new RegExp(`^Workspace w-a, ${words}(,|$)`));
  expect(screen.getByTestId("workspace-tray-overflow")).toHaveAccessibleName(
    new RegExp(`: ${words}(,|$)`),
  );
}

/** Quiet states remain visible on tab and workspace chips; attention roll-ups
 *  in tiles and overflow buttons leave them out. */
function expectQuietState(mark: StatusMarkKind) {
  const all = surfaces();
  for (const where of ["tab pill", "tray chip"]) {
    expect(all[where], where).toHaveAttribute("data-mark", mark);
  }
  for (const where of ['tile "+N"', "tile roll-up", 'tray "+N"'])
    expect(all[where], where).toBeNull();
  expect(screen.queryByTestId("tile-rollup-roll")).toBeNull();
}

function expectFinishedReady() {
  expectQuietState("ready");
  expect(screen.queryByTestId("tab-p1-ai-status")).toHaveAccessibleName(/^Ready/);
  expect(document.querySelector('[data-mark="done"]')).toBeNull();
}

/** Nothing moves: no turning arc and no pulsing halo on any surface. */
function expectStill() {
  for (const [where, el] of Object.entries(surfaces())) {
    expect(el!.querySelector(".cs-mark-spin, .cs-mark-halo"), where).toBeNull();
  }
}

/** The process ended cleanly: retained tab and tray marks show Ended. */
function expectEndedTab() {
  expectQuietState("ended");
}

/** The tab's own process crashed: every surface shows the red Crashed square. */
function expectCrashedTab() {
  for (const [where, el] of Object.entries(surfaces())) {
    expect(el, where).toHaveAttribute("data-mark", "crashed");
  }
}

function emit(name: string, payload: unknown) {
  const set = h.handlers.get(name);
  if (!set?.size) throw new Error(`no handler registered for ${name}`);
  act(() => {
    for (const cb of [...set]) cb({ payload });
  });
}
const hook = (hook_event: string, status: string, session = S) =>
  emit("terminal-status-update", { tab_session_id: session, status, hook_event });
const mcp = (agent_signal: unknown, status: string) =>
  emit("terminal-status-update", { tab_session_id: S, status, source: "mcp", agent_signal });

installTerminalLayoutHarness();

const CHIP_W = 100;
const IDS = ["w-a", "w-b", "w-c", "w-d", "w-e"];

beforeEach(() => {
  resetSessionProjection();
  h.handlers.clear();
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  resetLivenessPollerForTests();
  resetPrunedAgentSessionsForTests();
  resetDotSinceForTests();
  useAgentStateStore.setState({ bySession: {}, exitBySession: {}, stateSince: {}, seenAt: {} });
  // A clock that always moves, so "seen" and "entered" never tie.
  let now = 1_000;
  vi.spyOn(Date, "now").mockImplementation(() => ++now);
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: "w-b" },
    workspaces: { [TEAM]: IDS.map(ws) },
    softClosedWorkspaces: { [TEAM]: {} },
    mruWorkspaceStack: {},
    trayOrderByTeam: {},
    workspaceTraySeenCoachmark: true,
    aiStatusBySession: {},
  });
  projectSessionStatus(S, "unknown", { handle: "p1" });
  seedTerminalTabs(TEAM, "w-a", [S]);
  seedTerminalTabs(TEAM, "w-b", ["sess-b"]);
  seedTerminalTabs(TEAM, "w-c", ["sess-c"]);
  seedTerminalTabs(TEAM, "w-d", ["sess-d"]);
  seedTerminalTabs(TEAM, "w-e", [S, PLAIN]);
  // 250px of tray strip, 100px chips: w-a and w-b in view, w-c..w-e behind "+3".
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
    this: HTMLElement,
  ) {
    const chip = this.dataset.trayChip;
    const left = chip ? IDS.indexOf(chip) * CHIP_W : 0;
    const width = chip ? CHIP_W : 250;
    return { left, right: left + width, top: 0, bottom: 32, width, height: 32, x: left, y: 0 };
  } as () => DOMRect);
  renderHook(() => useTerminalStatusEvents());
  stopDotSince = watchDotSince();
});
let stopDotSince: () => void = () => {};
afterEach(() => {
  stopDotSince();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  resetLivenessPollerForTests();
  useAgentStateStore.setState({ bySession: {}, exitBySession: {} });
  useViewerStore.setState({ aiStatusBySession: {} });
});

describe("the dot projects the session status on every surface", () => {
  it("walks a projected session through every state; every surface moves together", () => {
    renderSurfaces();
    for (const [effective, terminal, mark] of [
      ["thinking", "thinking", "thinking"],
      ["tool-running", "running", "tool"],
      ["thinking", "thinking", "thinking"],
      ["paused", "needs-permission", "permission"],
      ["tool-running", "running", "tool"],
      ["thinking", "thinking", "thinking"],
    ] as const) {
      act(() => {
        projectSessionStatus(S, effective);
      });
      expectEverySurface(terminal, mark);
    }
    act(() => {
      projectSessionStatus(S, "idle");
    });
    expectFinishedReady();
    act(() => {
      projectSessionStatus(S, "failed");
    });
    expectEverySurface("error");
  });

  it("a crash reads crashed, apart from a declared error, and holds after the agent is gone", () => {
    renderSurfaces();
    hook("UserPromptSubmit", "running");
    hook("PreToolUse", "running");
    // kill -9: the shell reports the agent's command ended with 137.
    emit("pty-osc133", { session_id: S, kind: "command-end", exit_code: 137 });
    expect(useAgentStateStore.getState().bySession[S]?.state).toBe("crashed");
    // The stored status keeps the coarse wire literal; the dot is finer.
    expect(useViewerStore.getState().aiStatusBySession[S]).toBe("error");
    act(() => {
      projectSessionStatus(S, "crashed");
    });
    expectEverySurface("crashed");
  });

  it("SessionEnd leaves a Ready mark for the live prompt", () => {
    renderSurfaces();
    hook("UserPromptSubmit", "running");
    hook("SessionEnd", "idle");
    act(() => {
      projectSessionStatus(S, "idle");
    });
    expectFinishedReady();
  });

  describe("once the tab's own process has exited, a record that did not end on it is silent", () => {
    const ptyExit = (exit_code: number | null, signal: string | null = null) =>
      emit("pty-exit", { session_id: S, exit_code, signal, requested: false });

    it("an MCP-only agent's progress, then its shell exits 0: no stuck busy dot", () => {
      renderSurfaces();
      mcp({ kind: "signal-progress", phase: "dev" }, "running");
      act(() => {
        projectSessionStatus(S, "thinking");
      });
      expectEverySurface("thinking");
      ptyExit(0);
      expectEndedTab();
    });

    it("an open ask_user, then the shell exits 1: no needs-you on a dead tab", () => {
      renderSurfaces();
      mcp({ kind: "ask-user" }, "needs-permission");
      act(() => {
        projectSessionStatus(S, "paused");
      });
      expectEverySurface("needs-permission");
      ptyExit(1);
      expectEndedTab();
    });

    it("a finished Claude, then a SIGHUP ends the tab: no done on an ended tab", () => {
      renderSurfaces();
      hook("UserPromptSubmit", "running");
      hook("Stop", "done");
      act(() => {
        projectSessionStatus(S, "idle");
      });
      expectFinishedReady();
      ptyExit(null, "SIGHUP");
      expectCrashedTab();
    });

    it("a Claude killed mid-turn still reads crashed: that record ended on the exit", () => {
      renderSurfaces();
      hook("UserPromptSubmit", "running");
      ptyExit(null, "SIGKILL");
      expectCrashedTab();
    });
  });

  it("a crash and an error hold still, seen or not: only Needs you and Working move", () => {
    renderSurfaces();
    hook("UserPromptSubmit", "running");
    emit("pty-osc133", { session_id: S, kind: "command-end", exit_code: 137 });
    act(() => {
      projectSessionStatus(S, "crashed");
    });
    expectEverySurface("crashed");
    expectStill();
    act(() => markSessionSeen(S));
    expectEverySurface("crashed");
    expectStill();

    hook("UserPromptSubmit", "running");
    mcp({ kind: "signal-done", outcome: "failed" }, "error");
    act(() => {
      projectSessionStatus(S, "failed");
    });
    expectEverySurface("error");
    expectStill();
  });

  it("a finished turn reads Ready at once, seen or not, with quiet tile roll-ups", () => {
    renderSurfaces();
    hook("UserPromptSubmit", "running");
    hook("Stop", "done");
    act(() => {
      projectSessionStatus(S, "idle");
    });
    expectFinishedReady();
    act(() => markSessionSeen(S));
    expectFinishedReady();
  });

  it("a seen busy dot that the idle detector turns into needs-you is unseen again", () => {
    renderSurfaces();
    hook("UserPromptSubmit", "running");
    hook("PreToolUse", "running");
    act(() => {
      projectSessionStatus(S, "tool-running");
    });
    act(() => markSessionSeen(S));
    expect(isSessionSeen(useAgentStateStore.getState(), S)).toBe(true);
    // The idle-output detector writes the stored status only.
    act(() => useViewerStore.getState().setSessionAiStatus(S, "needs-permission"));
    act(() => {
      projectSessionStatus(S, "paused");
    });
    expectEverySurface("needs-permission");
    expect(isSessionSeen(useAgentStateStore.getState(), S)).toBe(false);
  });

  it("a projected failure holds steady without a local record, seen or not", () => {
    renderSurfaces();
    act(() => useViewerStore.getState().setSessionAiStatus(S, "error"));
    expect(useAgentStateStore.getState().bySession[S]).toBeUndefined();
    act(() => {
      projectSessionStatus(S, "failed");
    });
    expectEverySurface("error");
    act(() => markSessionSeen(S));
    expectEverySurface("error");
    expectStill();
  });

  it("Needs you's halo pulses on every surface, even once seen; Working's arc turns", () => {
    renderSurfaces();
    hook("PermissionRequest", "needs-permission");
    act(() => {
      projectSessionStatus(S, "paused");
    });
    act(() => markSessionSeen(S));
    expectEverySurface("needs-permission");
    for (const el of Object.values(surfaces())) {
      expect(el!.querySelector(".cs-mark-halo")).not.toBeNull();
    }
    hook("PostToolUse", "running");
    hook("PreToolUse", "running");
    act(() => {
      projectSessionStatus(S, "tool-running");
    });
    for (const el of Object.values(surfaces())) {
      expect(el!.querySelector(".cs-mark-spin")).not.toBeNull();
    }
  });

  it("projected unknown activity stays quiet while a failure remains actionable", () => {
    renderSurfaces();
    act(() => useViewerStore.getState().setSessionAiStatus(S, "running"));
    expect(useAgentStateStore.getState().bySession[S]).toBeUndefined();
    act(() => {
      projectSessionStatus(S, "unknown");
    });
    expect(surfaces()["tab pill"]).toHaveAttribute("data-mark", "unknown");
    expect(surfaces()["tab pill"]).toHaveAccessibleName(/^Activity unknown/);
    expectQuietState("unknown");
    act(() => useViewerStore.getState().setSessionAiStatus(S, "error"));
    act(() => {
      projectSessionStatus(S, "failed");
    });
    expectEverySurface("error");
  });

  it("the roll-ups rank the finer states: a crash outranks an error, work outranks thought", () => {
    renderSurfaces();
    // w-e holds S and a plain shell: its dot is the higher of the two.
    act(() => useViewerStore.getState().setSessionAiStatus(PLAIN, "error"));
    hook("UserPromptSubmit", "running");
    act(() => {
      projectSessionStatus(PLAIN, "failed");
      projectSessionStatus(S, "thinking");
    });
    const trayPlus = () => screen.getByTestId("workspace-tray-overflow-status");
    expect(paints(trayPlus(), "error")).toBe(true);
    emit("pty-osc133", { session_id: S, kind: "command-end", exit_code: 137 });
    act(() => {
      projectSessionStatus(S, "crashed");
    });
    expect(paints(trayPlus(), "crashed")).toBe(true);

    act(() => useViewerStore.getState().setSessionAiStatus(PLAIN, "running"));
    hook("UserPromptSubmit", "running");
    act(() => {
      projectSessionStatus(PLAIN, "tool-running");
      projectSessionStatus(S, "thinking");
    });
    expect(trayPlus()).toHaveAttribute("data-mark", "tool");
  });
});

describe("a tool call in one tab re-renders no unrelated dot host", () => {
  it("the other session's dot, seen flag and roll-up hosts stay put", () => {
    let renders = 0;
    function OtherHost() {
      renders++;
      const status = useSessionAiStatus(PLAIN);
      const seen = useScopeMark({ sessionIds: [PLAIN] }).mark;
      const tray = useScopeMark({ teamKey: TEAM, wsIds: ["w-b"] }).mark;
      return <span data-testid="other">{`${status}-${seen}-${tray}`}</span>;
    }
    render(<OtherHost />);
    const before = renders;
    hook("UserPromptSubmit", "running");
    hook("PreToolUse", "running");
    hook("PostToolUse", "running");
    for (const status of ["thinking", "tool-running", "thinking"] as const)
      act(() => {
        projectSessionStatus(S, status);
      });
    act(() => markSessionSeen(S));
    expect(renders).toBe(before);
    // …and it does re-render for its own session.
    act(() => {
      projectSessionStatus(PLAIN, "tool-running");
    });
    expect(renders).toBe(before + 1);
  });
});
