// SPDX-License-Identifier: MIT
import { projectSessionStatus } from "@/test/session-projection-fixture";
import { installUserNavigationCapture } from "@/shared/lib/context-navigation";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, renderHook } from "@testing-library/react";
import { Toaster } from "sonner";

import { hideTab } from "@/features/viewer/lib/terminal-close";
import { resetDotSinceForTests } from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import {
  resetManagedSessionStoreForTests,
  useManagedSessionStore,
} from "@/shared/stores/use-managed-session-store";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  selectActiveTerminalTab,
  selectHiddenTerminalTabs,
  useViewerStore,
  type Workspace,
} from "@/shared/stores/viewer-store";
import { activateAndFocus } from "./focus-cascade";
import { focusTileFrame, focusTileQuestionBanner } from "./tile-question-focus";
import { useTileTabActions } from "../components/workspace-terminal-panels";
import { switchContext } from "./context-switch";
import { focusTileKey } from "./focus-indicator";
import { TILE_QUESTION_ATTR, TILE_QUESTION_ASK_ATTR } from "./tile-question-focus";

/**
 * The seam: real stores → the controller → the OS boundary, and the return →
 * landing / Jump pill. Only the Tauri notification plugin and the Tauri window
 * are faked (and the IPC the store's own actions persist through). The pill is
 * the real app toast.
 */

const os = vi.hoisted(() => ({
  isPermissionGranted: vi.fn(),
  requestPermission: vi.fn(),
  sendNotification: vi.fn(),
  setBadgeCount: vi.fn(),
  setFocus: vi.fn(),
  focusCb: null as null | ((e: { payload: boolean }) => void),
}));

vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: os.isPermissionGranted,
  requestPermission: os.requestPermission,
  sendNotification: os.sendNotification,
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    isFocused: () => Promise.resolve(true),
    onFocusChanged: (cb: (e: { payload: boolean }) => void) => {
      os.focusCb = cb;
      return Promise.resolve(() => {});
    },
    setBadgeCount: (n?: number) => {
      os.setBadgeCount(n);
      return Promise.resolve();
    },
    unminimize: () => Promise.resolve(),
    show: () => Promise.resolve(),
    setFocus: () => {
      os.setFocus();
      return Promise.resolve();
    },
  }),
}));

const shown = vi.hoisted(() => ({
  showAgent: vi.fn(
    async (): Promise<{ shown: "adopted" } | { notShown: string }> => ({
      shown: "adopted",
    }),
  ),
}));
vi.mock("@/features/viewer/lib/show-agent", () => ({ showAgent: shown.showAgent }));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  killPty: vi.fn().mockResolvedValue(undefined),
}));

const {
  startAttentionAlerts,
  routeToAttention,
  getPendingAttentionTarget,
  BURST_MS,
  COOLDOWN_MS,
  LAND_WINDOW_MS,
  POINTER_GRACE_MS,
  RENOTIFY_FLOOR_MS,
} = await import("./attention-alerts-controller");
const { resetOsAttentionForTests, systemNotificationSound } = await import("./os-attention");
const { useAgentListProjectionStore } = await import("@/features/viewer/lib/agent-list-projection");
const { getAgentStatusOf } = await import("@/features/viewer/hooks/use-agent-status-of");
const { isNeedsYouStatus } = await import("@/shared/lib/agent-status-config");

// Captured before any fake timers: real waits for animation frames and React.
const realSetTimeout = globalThis.setTimeout;
const realWait = (ms = 20) => new Promise<void>((r) => realSetTimeout(r, ms));
async function eventually(check: () => void, tries = 50) {
  for (let i = 0; ; i++) {
    try {
      check();
      return;
    } catch (e) {
      if (i >= tries) throw e;
      await act(async () => {
        await realWait(10);
        if (vi.isFakeTimers()) await vi.advanceTimersByTimeAsync(50);
      });
    }
  }
}

installTerminalLayoutHarness();

const TEAM = "acme:default";
/** A tab seeded in the terminal host: session `sid-<x>`, named "Tab <x>". */
const tab = (x: string) => ({ session: `sid-${x}`, name: `Tab ${x}` });
const ws = (id: string): Workspace => ({
  id,
  kind: "story",
  cwd: `/${id}`,
  status: "ready",
  storyId: id.toUpperCase(),
});

function seed() {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  useViewerStore.setState({
    workspaces: { [TEAM]: [ws("w1"), ws("w2")] },
    activeWorkspace: { [TEAM]: "w1" },
    softClosedWorkspaces: { [TEAM]: {} },
    aiStatusBySession: {},
    progressBySession: {},
    terminalVisible: true,
    foregroundLayout: {},
    settings: {
      ...useViewerStore.getState().settings,
      osNotifications: undefined,
      notifyOnFinish: undefined,
      notificationSound: undefined,
    },
  });
  seedTerminalTabs(TEAM, "w1", [tab("a"), tab("b")], { active: "sid-a" });
  seedTerminalTabs(TEAM, "w2", [tab("c"), tab("h")], { active: "sid-c" });
  useAgentStateStore.setState({ bySession: {}, exitBySession: {}, stateSince: {}, seenAt: {} });
  resetDotSinceForTests();
  resetManagedSessionStoreForTests();
  useAgentListProjectionStore.setState({ sessions: null });
}

/** Hide a seeded tab from its tile (a Background entry of its workspace). */
async function hide(workspaceId: string, session: string) {
  hideTab(TEAM, workspaceId, session);
  await vi.advanceTimersByTimeAsync(0);
  expect(
    selectHiddenTerminalTabs(useViewerStore.getState(), TEAM, workspaceId).map((t) => t.id),
  ).toEqual([session]);
}

/** Mark a session's current state seen, as the watched-tab hook does. */
function markSeen(session: string) {
  useAgentStateStore.setState((a) => ({
    stateSince: { ...a.stateSince, [session]: 1 },
    seenAt: { ...a.seenAt, [session]: 2 },
  }));
}

type Wire = Parameters<ReturnType<typeof useViewerStore.getState>["setSessionAiStatus"]>[1];
const setStatus = (sid: string, status: Wire) =>
  useViewerStore.getState().setSessionAiStatus(sid, status);
const ask = (n: number, sid: string, question = "Ship it?") =>
  useManagedSessionStore
    .getState()
    .addPendingAsk({ correlationId: `ask-user-${n}`, sessionId: sid, question });
/** An agent (it announced itself with a hook) whose process then died abnormally. */
const crash = (sid: string) => {
  useAgentStateStore
    .getState()
    .recordSignal(sid, { tier: 1, signal: { kind: "claude-hook", event: "UserPromptSubmit" } });
  useAgentStateStore.getState().recordPtyExit(sid, { code: 139, signal: null, requested: false });
};

const setFocused = (f: boolean) => os.focusCb?.({ payload: f });
const settle = () => vi.advanceTimersByTimeAsync(0);
const lastBadge = () => os.setBadgeCount.mock.calls.at(-1)?.[0];
const titles = () => os.sendNotification.mock.calls.map((c) => (c[0] as { title: string }).title);
const view = () => {
  const v = useViewerStore.getState();
  const wsId = v.activeWorkspace[TEAM];
  return { ws: wsId, tab: selectActiveTerminalTab(v, TEAM, wsId) };
};

let stop: () => void = () => {};

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
  os.isPermissionGranted.mockReset().mockResolvedValue(true);
  os.requestPermission.mockReset().mockResolvedValue("granted");
  os.sendNotification.mockReset();
  os.setBadgeCount.mockReset();
  os.setFocus.mockReset();
  os.focusCb = null;
  shown.showAgent.mockClear();
  resetOsAttentionForTests();
  seed();
});

afterEach(() => {
  stop();
  vi.useRealTimers();
  cleanup();
  document.body.innerHTML = "";
});

async function start() {
  stop = startAttentionAlerts();
  await settle();
}

describe("OS notifications fire only for needs-you states, unfocused, once per entry", () => {
  it("notifies a self-reported block with its current reason and clears it on activity", async () => {
    await start();
    setFocused(false);
    useViewerStore
      .getState()
      .setSessionProgress("sid-b", { phase: "blocked", note: "Missing credentials" });
    useAgentStateStore.getState().recordSignal("sid-b", {
      tier: 1,
      signal: { kind: "signal-progress", phase: "blocked" },
    });
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(os.sendNotification.mock.calls.map((c) => c[0])).toEqual([
      { title: "W1 · Tab b is blocked", body: "Blocked: Missing credentials" },
    ]);
    expect(lastBadge()).toBe(1);
    useAgentStateStore.getState().recordSignal("sid-b", {
      tier: 1,
      signal: { kind: "claude-hook", event: "UserPromptSubmit" },
    });
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(lastBadge()).toBeUndefined();
    expect(os.sendNotification).toHaveBeenCalledTimes(1);
  });

  it("unfocused + a permission prompt → one notification naming the workspace, tab and state", async () => {
    await start();
    setFocused(false);
    setStatus("sid-b", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(titles()).toEqual(["W1 · Tab b needs your permission"]);
  });

  it.each<[string, () => void]>([
    ["working", () => setStatus("sid-a", "running")],
    ["done", () => setStatus("sid-a", "done")],
    [
      "a plain shell killed by a signal (no agent ran)",
      () =>
        useAgentStateStore
          .getState()
          .recordPtyExit("sid-a", { code: null, signal: "SIGKILL", requested: false }),
    ],
  ])("unfocused + %s → no notification, no badge", async (_label, act) => {
    await start();
    setFocused(false);
    act();
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
    expect(lastBadge()).toBeUndefined();
  });

  it("a declared failure counts on the badge and notifies that the agent hit an error", async () => {
    await start();
    setFocused(false);
    setStatus("sid-a", "error");
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification.mock.calls.map((c) => c[0])).toEqual([
      { title: "W1 · Tab a hit an error", body: "The agent stopped on an error." },
    ]);
    expect(lastBadge()).toBe(1);
  });

  it("an API error from a StopFailure hook notifies once, with its reason", async () => {
    await start();
    setFocused(false);
    useAgentStateStore
      .getState()
      .recordSignal("sid-a", { tier: 1, signal: { kind: "claude-hook", event: "StopFailure" } });
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    useViewerStore.setState({ aiStatusBySession: {} }); // an unrelated tick
    await vi.advanceTimersByTimeAsync(COOLDOWN_MS);
    expect(os.sendNotification.mock.calls.map((c) => c[0])).toEqual([
      { title: "W1 · Tab a hit an error", body: "The agent stopped on an error: API error" },
    ]);
  });

  it.each([false, true])(
    "uses only an eligible completion summary for current failure (continued=%s)",
    async (continued) => {
      await start();
      setFocused(false);
      useManagedSessionStore
        .getState()
        .applyDoneStatus("sid-a", "failed", { summary: "Old failure" });
      if (continued)
        useAgentStateStore.getState().recordSignal("sid-a", {
          tier: 1,
          signal: { kind: "claude-hook", event: "UserPromptSubmit" },
        });
      useAgentStateStore
        .getState()
        .recordSignal("sid-a", { tier: 1, signal: { kind: "claude-hook", event: "StopFailure" } });
      await vi.advanceTimersByTimeAsync(BURST_MS * 2);
      expect(os.sendNotification.mock.calls.map((c) => c[0].body)).toEqual([
        `The agent stopped on an error: ${continued ? "API error" : "Old failure"}`,
      ]);
      expect(useManagedSessionStore.getState().summaries["sid-a"]).toBe("Old failure");
    },
  );

  it("an observed API error names its kind", async () => {
    await start();
    setFocused(false);
    useAgentStateStore.getState().recordActivity(
      {
        session: "sid-a",
        identity: { hostBootId: "boot", sessionIncarnation: "inc", invocationId: "run" },
        revision: 2,
        turnGeneration: 1,
        provider: "claude",
        state: "errored",
        observed: true,
        limited: false,
        exit: null,
        asks: [],
        reason: "rate_limit",
      },
      Date.now(),
    );
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification.mock.calls.map((c) => c[0])).toEqual([
      { title: "W1 · Tab a hit an error", body: "The agent stopped on an error: Rate limited" },
    ]);
  });

  it("an ask_user question and an agent crash each notify, in words", async () => {
    await start();
    setFocused(false);
    ask(1, "sid-a", "Merge the PR now?");
    await vi.advanceTimersByTimeAsync(BURST_MS + COOLDOWN_MS);
    crash("sid-c");
    await vi.advanceTimersByTimeAsync(COOLDOWN_MS);
    expect(os.sendNotification.mock.calls.map((c) => c[0])).toEqual([
      { title: "W1 · Tab a has a question for you", body: "“Merge the PR now?”" },
      { title: "W2 · Tab c crashed", body: "The agent crashed (exit code 139)." },
    ]);
  });

  it("focused → nothing fires (the badge still updates), and leaving later does not replay it", async () => {
    await start();
    setStatus("sid-b", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
    expect(lastBadge()).toBe(1);
    setFocused(false);
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
  });

  it("a status tick in the same state does not re-notify", async () => {
    await start();
    setFocused(false);
    setStatus("sid-b", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS);
    useViewerStore.setState({ aiStatusBySession: { "sid-b": "needs-permission" } }); // new ref
    await vi.advanceTimersByTimeAsync(COOLDOWN_MS);
    expect(os.sendNotification).toHaveBeenCalledTimes(1);
  });

  it("a flapping tab re-notifies only after the floor, or once the user has been back", async () => {
    await start();
    setFocused(false);
    const flap = async () => {
      setStatus("sid-b", "running");
      await settle();
      setStatus("sid-b", "needs-permission");
      await vi.advanceTimersByTimeAsync(COOLDOWN_MS);
    };
    setStatus("sid-b", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS);
    await flap(); // 10 s later: inside the floor
    await flap(); // 20 s
    expect(os.sendNotification).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(RENOTIFY_FLOOR_MS);
    await flap(); // past the floor
    expect(os.sendNotification).toHaveBeenCalledTimes(2);
    setFocused(true); // the user comes back (late: no landing)…
    await settle();
    setFocused(false);
    await flap(); // …so the next entry notifies even inside the floor
    expect(os.sendNotification).toHaveBeenCalledTimes(3);
  });

  it("a burst from several tabs collapses into one summary", async () => {
    await start();
    setFocused(false);
    setStatus("sid-a", "needs-permission");
    await vi.advanceTimersByTimeAsync(200);
    ask(1, "sid-b");
    await vi.advanceTimersByTimeAsync(200);
    crash("sid-c");
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(os.sendNotification.mock.calls.map((c) => c[0])).toEqual([
      {
        title: "3 agents need you",
        body: [
          "W1 · Tab a needs your permission",
          "W1 · Tab b has a question for you",
          "W2 · Tab c crashed",
        ].join("\n"),
      },
    ]);
  });

  it("is throttled: what arrives during the cooldown is summarised once it ends", async () => {
    await start();
    setFocused(false);
    setStatus("sid-a", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS);
    setStatus("sid-b", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS);
    setStatus("sid-c", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(os.sendNotification).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(COOLDOWN_MS);
    expect(titles()).toEqual(["W1 · Tab a needs your permission", "2 agents need you"]);
  });

  it("an entry resolved before the burst window ends never fires", async () => {
    await start();
    setFocused(false);
    setStatus("sid-a", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS / 2);
    setStatus("sid-a", "running");
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(os.sendNotification).not.toHaveBeenCalled();
  });

  it("the setting off → no notification, the badge still counts", async () => {
    useViewerStore.getState().updateSettings({ osNotifications: false });
    await start();
    setFocused(false);
    setStatus("sid-a", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
    expect(lastBadge()).toBe(1);
  });
});

describe("permission", () => {
  it("is not asked at startup, only on the first qualifying event", async () => {
    os.isPermissionGranted.mockResolvedValue(false);
    await start();
    setFocused(false);
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.isPermissionGranted).not.toHaveBeenCalled();
    expect(os.requestPermission).not.toHaveBeenCalled();
    setStatus("sid-a", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(os.requestPermission).toHaveBeenCalledTimes(1);
    expect(os.sendNotification).toHaveBeenCalledTimes(1);
  });

  it("denied → no notification, no throw, never re-asked, and the badge still works", async () => {
    os.isPermissionGranted.mockResolvedValue(false);
    os.requestPermission.mockResolvedValue("denied");
    await start();
    setFocused(false);
    setStatus("sid-a", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS);
    setStatus("sid-b", "needs-permission");
    await vi.advanceTimersByTimeAsync(COOLDOWN_MS);
    expect(os.sendNotification).not.toHaveBeenCalled();
    expect(os.requestPermission).toHaveBeenCalledTimes(1);
    expect(lastBadge()).toBe(2);
  });
});

/** A tile frame with a live terminal (and optionally a question banner). */
function mountTile(
  workspaceId: string,
  opts: { question?: boolean; collapsed?: boolean; teamKey?: string; outsideCli?: boolean } = {},
) {
  const frame = document.createElement("div");
  frame.setAttribute("data-pane-id", workspaceId);
  frame.setAttribute("data-focus-tile", focusTileKey(opts.teamKey ?? TEAM, workspaceId));
  if (opts.collapsed) frame.setAttribute("data-zoom", "collapsed");
  const xterm = document.createElement("div");
  xterm.className = "xterm";
  const input = document.createElement("textarea");
  xterm.appendChild(input);
  frame.appendChild(xterm);
  if (opts.question) {
    const banner = document.createElement("div");
    banner.setAttribute(TILE_QUESTION_ATTR, "");
    banner.setAttribute(TILE_QUESTION_ASK_ATTR, "ask-user-1");
    const option = document.createElement("button");
    option.setAttribute("data-testid", "ask-option");
    option.textContent = "Yes";
    banner.appendChild(option);
    frame.appendChild(banner);
  }
  let host = document.querySelector("[data-xterm-host]");
  if (!host) {
    host = document.createElement("div");
    host.setAttribute("data-xterm-host", "");
    document.body.appendChild(host);
  }
  (opts.outsideCli ? document.body : host).appendChild(frame);
  return { frame, input };
}

describe("the dock badge counts tabs that need you", () => {
  it("counts the three states, updates as they resolve, and clears to nothing at zero", async () => {
    await start();
    expect(lastBadge()).toBeUndefined(); // cleared at start
    setFocused(false);
    setStatus("sid-a", "needs-permission");
    ask(1, "sid-b");
    crash("sid-c");
    await settle();
    expect(lastBadge()).toBe(3);
    setStatus("sid-a", "running");
    await settle();
    expect(lastBadge()).toBe(2);
    useManagedSessionStore.getState().resolvePendingAsk("ask-user-1");
    await settle();
    expect(lastBadge()).toBe(1);
  });

  it("counts a Hidden tab", async () => {
    await hide("w1", "sid-b");
    await start();
    setStatus("sid-b", "needs-permission");
    await settle();
    expect(lastBadge()).toBe(1);
  });

  it("a seen crash still counts until it is dismissed: the agent is still stuck", async () => {
    await start();
    setFocused(false);
    crash("sid-c");
    await settle();
    expect(lastBadge()).toBe(1);
    markSeen("sid-c");
    await settle();
    expect(lastBadge()).toBe(1);
    projectSessionStatus("sid-c", "done", { crashId: "dismissed" });
    await settle();
    expect(lastBadge()).toBeUndefined();
  });

  it("a seen error still counts until its agent starts a new turn", async () => {
    await start();
    useAgentStateStore
      .getState()
      .recordSignal("sid-c", { tier: 1, signal: { kind: "claude-hook", event: "StopFailure" } });
    await settle();
    markSeen("sid-c");
    await settle();
    expect(lastBadge()).toBe(1);
    useAgentStateStore.getState().recordSignal("sid-c", {
      tier: 1,
      signal: { kind: "claude-hook", event: "UserPromptSubmit" },
    });
    await settle();
    expect(lastBadge()).toBeUndefined();
  });

  it("a seen question still counts: it blocks until answered", async () => {
    await start();
    ask(1, "sid-c");
    await settle();
    markSeen("sid-c");
    await settle();
    expect(lastBadge()).toBe(1);
  });
});

describe("the return after a notification", () => {
  /** Notify for `act_`, then come back `after` ms later. */
  async function notifyThenReturn(act_: () => void, after = 1000) {
    await start();
    setFocused(false);
    act_();
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(os.sendNotification).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(after);
    setFocused(true);
  }
  const passGrace = () => vi.advanceTimersByTimeAsync(POINTER_GRACE_MS + 1);
  const bannerOf = (frame: HTMLElement) =>
    frame.querySelector<HTMLElement>(`[${TILE_QUESTION_ATTR}]`)!;

  describe("a quick return that looks like the notification click lands — never in the terminal", () => {
    it("selects the workspace and tab and parks focus on its tile frame", async () => {
      mountTile("w1");
      const { frame } = mountTile("w2");
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"));
      await passGrace();
      await eventually(() => expect(document.activeElement).toBe(frame));
      expect(view()).toEqual({ ws: "w2", tab: "sid-c" });
      expect(os.setFocus).toHaveBeenCalled();
    });

    it("⌘-Tab back while typing: the keys and Enter reach no agent", async () => {
      const { frame, input } = mountTile("w2", { question: true });
      const reachedTerminal: string[] = [];
      input.addEventListener("keydown", (e) => reachedTerminal.push(e.key));
      input.addEventListener("focus", () => reachedTerminal.push("(focus)"));
      const answered = vi.fn();
      bannerOf(frame).querySelector("button")!.addEventListener("click", answered);
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"));
      await passGrace();
      await eventually(() => expect(document.activeElement).toBe(frame));
      for (const key of ["y", "e", "s", "Enter"]) {
        fireEvent.keyDown(document.activeElement!, { key });
        fireEvent.keyUp(document.activeElement!, { key });
      }
      expect(reachedTerminal).toEqual([]);
      expect(answered).not.toHaveBeenCalled();
      expect(document.activeElement?.tagName).not.toMatch(/TEXTAREA|INPUT|BUTTON/);
    });

    it("a question: focus parks on the banner itself, not an answer", async () => {
      const { frame } = mountTile("w2", { question: true });
      await notifyThenReturn(() => ask(1, "sid-c"));
      await passGrace();
      await eventually(() => expect(document.activeElement).toBe(bannerOf(frame)));
      expect(view()).toEqual({ ws: "w2", tab: "sid-c" });
    });

    it("selects a tab that is not its tile's active one (pulling it out of overflow)", async () => {
      const { frame } = mountTile("w1");
      useViewerStore.getState().switchWorkspace(TEAM, "w2");
      await notifyThenReturn(() => setStatus("sid-b", "needs-permission"));
      await passGrace();
      await eventually(() => expect(view()).toEqual({ ws: "w1", tab: "sid-b" }));
      await eventually(() => expect(document.activeElement).toBe(frame));
    });

    it("waits out the click grace before moving anything", async () => {
      mountTile("w2");
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"));
      await vi.advanceTimersByTimeAsync(POINTER_GRACE_MS - 10);
      expect(view()).toEqual({ ws: "w1", tab: "sid-a" });
    });

    it("restores a Hidden tab onto its tile", async () => {
      await hide("w1", "sid-b");
      const { frame } = mountTile("w1");
      await notifyThenReturn(() => setStatus("sid-b", "needs-permission"));
      await passGrace();
      await eventually(() => expect(view()).toEqual({ ws: "w1", tab: "sid-b" }));
      await eventually(() =>
        expect(selectHiddenTerminalTabs(useViewerStore.getState(), TEAM, "w1")).toEqual([]),
      );
      await eventually(() => expect(document.activeElement).toBe(frame));
    });

    it("does nothing when the tab no longer needs the user", async () => {
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"), 0);
      setStatus("sid-c", "running");
      await settle();
      await passGrace();
      expect(view()).toEqual({ ws: "w1", tab: "sid-a" });
      expect(getPendingAttentionTarget()).toBeNull();
    });

    it("does nothing when the notification was not shown", async () => {
      os.isPermissionGranted.mockResolvedValue(false);
      os.requestPermission.mockResolvedValue("denied");
      mountTile("w2");
      await start();
      setFocused(false);
      setStatus("sid-c", "needs-permission");
      await vi.advanceTimersByTimeAsync(BURST_MS + 500);
      setFocused(true);
      await passGrace();
      expect(view()).toEqual({ ws: "w1", tab: "sid-a" });
      expect(getPendingAttentionTarget()).toBeNull();
    });
  });

  describe("any other return moves nothing and offers a Jump pill", () => {
    beforeEach(() => {
      render(<Toaster />);
    });
    const PILL = "W2 · Tab c needs your permission";

    async function expectPillNotMoved() {
      await passGrace();
      expect(view()).toEqual({ ws: "w1", tab: "sid-a" });
      await eventually(() => expect(screen.getByText(PILL)).toBeInTheDocument());
      expect(getPendingAttentionTarget()).toBe("sid-c");
    }
    const late = LAND_WINDOW_MS + 1;
    const pillGone = () =>
      eventually(() => expect(screen.queryByText(PILL)).not.toBeInTheDocument());

    it("(a) focus comes back after the land window", async () => {
      mountTile("w2");
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"), late);
      await expectPillNotMoved();
    });

    it("(b) a pointer went down in the window just after focus returned", async () => {
      mountTile("w2");
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"));
      await vi.advanceTimersByTimeAsync(50);
      fireEvent.pointerDown(document.body);
      await expectPillNotMoved();
    });

    it("(b) a pointer went down just before the focus event (the click that focused)", async () => {
      mountTile("w2");
      await start();
      setFocused(false);
      setStatus("sid-c", "needs-permission");
      await vi.advanceTimersByTimeAsync(BURST_MS + 500);
      fireEvent.pointerDown(document.body);
      await vi.advanceTimersByTimeAsync(40);
      setFocused(true);
      await expectPillNotMoved();
    });

    it("(c) a dialog is open", async () => {
      mountTile("w2");
      const dialog = document.createElement("div");
      dialog.setAttribute("role", "dialog");
      document.body.appendChild(dialog);
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"));
      await expectPillNotMoved();
    });

    it("sits bottom-center, clear of the tile controls, with a ⌘⇧J hint", async () => {
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"), late);
      await expectPillNotMoved();
      const toaster = screen.getByText(PILL).closest("[data-sonner-toaster]")!;
      expect(toaster.getAttribute("data-y-position")).toBe("bottom");
      expect(toaster.getAttribute("data-x-position")).toBe("center");
      expect(screen.getByText("Jump there (⌘⇧J)")).toBeInTheDocument();
    });

    it("Jump selects the tab and parks focus on its tile frame, never the terminal", async () => {
      const { frame, input } = mountTile("w2");
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"), late);
      await expectPillNotMoved();
      fireEvent.click(screen.getByRole("button", { name: "Jump" }));
      await eventually(() => expect(document.activeElement).toBe(frame));
      expect(document.activeElement).not.toBe(input);
      expect(view()).toEqual({ ws: "w2", tab: "sid-c" });
      expect(getPendingAttentionTarget()).toBeNull();
    });

    it("Jump on a question parks on the banner, not an answer button", async () => {
      const { frame } = mountTile("w2", { question: true });
      await notifyThenReturn(() => ask(1, "sid-c"), late);
      await passGrace();
      await eventually(() => screen.getByRole("button", { name: "Jump" }));
      fireEvent.click(screen.getByRole("button", { name: "Jump" }));
      await eventually(() => expect(document.activeElement).toBe(bannerOf(frame)));
    });

    it("Jump restores a Hidden tab", async () => {
      await hide("w2", "sid-h");
      mountTile("w2");
      await notifyThenReturn(() => setStatus("sid-h", "needs-permission"), late);
      await passGrace();
      expect(view()).toEqual({ ws: "w1", tab: "sid-a" });
      await eventually(() => screen.getByRole("button", { name: "Jump" }));
      fireEvent.click(screen.getByRole("button", { name: "Jump" }));
      await eventually(() => expect(view()).toEqual({ ws: "w2", tab: "sid-h" }));
      await eventually(() =>
        expect(selectHiddenTerminalTabs(useViewerStore.getState(), TEAM, "w2")).toEqual([]),
      );
    });

    it("a Hidden tab's pill goes away once that tab is restored and selected another way (the live row, not the offered one)", async () => {
      await hide("w2", "sid-h");
      await notifyThenReturn(() => setStatus("sid-h", "needs-permission"), late);
      await passGrace();
      await eventually(() => expect(getPendingAttentionTarget()).toBe("sid-h"));
      useViewerStore.getState().restoreHiddenTerminalTab(TEAM, "w2", "sid-h");
      await eventually(() => expect(view()).toEqual({ ws: "w2", tab: "sid-h" }));
      await eventually(() => expect(getPendingAttentionTarget()).toBeNull());
      await eventually(() =>
        expect(screen.queryByText("W2 · Tab h needs your permission")).not.toBeInTheDocument(),
      );
    });

    it("landing on the offered tab another way (the jump cycle) dismisses the pill", async () => {
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"), late);
      await expectPillNotMoved();
      useViewerStore.getState().setActiveTerminalTab(TEAM, "sid-c", "w2");
      useViewerStore.getState().switchWorkspace(TEAM, "w2");
      await settle();
      expect(getPendingAttentionTarget()).toBeNull();
      await pillGone();
    });

    it("the pill goes away once the tab no longer needs the user", async () => {
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"), late);
      await expectPillNotMoved();
      setStatus("sid-c", "running");
      await settle();
      expect(getPendingAttentionTarget()).toBeNull();
      await pillGone();
    });

    it("a crash's pill goes away once the crash is seen", async () => {
      await notifyThenReturn(() => crash("sid-c"), late);
      await passGrace();
      await eventually(() => expect(getPendingAttentionTarget()).toBe("sid-c"));
      markSeen("sid-c");
      await eventually(() => expect(getPendingAttentionTarget()).toBeNull());
      await eventually(() =>
        expect(screen.queryByText("W2 · Tab c crashed")).not.toBeInTheDocument(),
      );
    });

    it("closing it with its close button clears the offer", async () => {
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"), late);
      await expectPillNotMoved();
      fireEvent.click(screen.getByRole("button", { name: /close/i }));
      await eventually(() => expect(getPendingAttentionTarget()).toBeNull());
      await pillGone();
    });

    it("Escape inside the toasts dismisses it; Escape in a terminal does not", async () => {
      const { input } = mountTile("w2");
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"), late);
      await expectPillNotMoved();
      input.focus();
      fireEvent.keyDown(input, { key: "Escape" });
      expect(getPendingAttentionTarget()).toBe("sid-c");
      const jump = screen.getByRole("button", { name: "Jump" });
      jump.focus();
      fireEvent.keyDown(jump, { key: "Escape" });
      expect(getPendingAttentionTarget()).toBeNull();
      await pillGone();
    });

    it("closing by itself clears the offer", async () => {
      await notifyThenReturn(() => setStatus("sid-c", "needs-permission"), late);
      await expectPillNotMoved();
      await act(() => vi.advanceTimersByTimeAsync(25_000));
      await eventually(() => expect(getPendingAttentionTarget()).toBeNull());
    });
  });
});

/** A roster row: an agent the Agents pane lists, here with no tab in the layout. */
const rosterAgent = (handle: string, sessionId: string, displayName: string) => ({
  handle,
  sessionId,
  kind: "agent",
  alive: true,
  backgrounded: false,
  startedWith: null,
  startedAtMs: null,
  launcher: null,
  lastOutputMs: null,
  displayName,
});
const setRoster = (...rows: ReturnType<typeof rosterAgent>[]) =>
  useAgentListProjectionStore.setState({ sessions: rows });

describe("an agent with no tab (another machine, a closed workspace) still alerts", () => {
  it("counts on the badge and notifies by the agent's name", async () => {
    setRoster(rosterAgent("reviewer", "sid-r", "Reviewer"));
    await start();
    setFocused(false);
    setStatus("sid-r", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(titles()).toEqual(["Reviewer needs your permission"]);
    expect(lastBadge()).toBe(1);
    setStatus("sid-r", "running");
    await settle();
    expect(lastBadge()).toBeUndefined();
  });

  it("a session listed both by its tab and on the roster counts and notifies once, by its tab", async () => {
    setRoster(rosterAgent("tab-b", "sid-b", "Builder"));
    await start();
    setFocused(false);
    ask(1, "sid-b", "Rebase first?");
    await vi.advanceTimersByTimeAsync(BURST_MS + COOLDOWN_MS);
    expect(titles()).toEqual(["W1 · Tab b has a question for you"]);
    expect(lastBadge()).toBe(1);
  });

  it("what already needed you when the roster first loads is not news", async () => {
    setStatus("sid-r", "needs-permission");
    await start();
    setFocused(false);
    setRoster(rosterAgent("reviewer", "sid-r", "Reviewer"));
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
    expect(lastBadge()).toBe(1);
  });

  it("an ended roster agent does not count", async () => {
    setRoster({ ...rosterAgent("reviewer", "sid-r", "Reviewer"), alive: false });
    await start();
    setFocused(false);
    setStatus("sid-r", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
  });

  describe("the return", () => {
    beforeEach(() => {
      render(<Toaster />);
    });

    async function notifyThenReturnLate() {
      setRoster(rosterAgent("reviewer", "sid-r", "Reviewer"));
      await start();
      setFocused(false);
      setStatus("sid-r", "needs-permission");
      await vi.advanceTimersByTimeAsync(BURST_MS);
      await vi.advanceTimersByTimeAsync(LAND_WINDOW_MS + 1);
      setFocused(true);
      await vi.advanceTimersByTimeAsync(POINTER_GRACE_MS + 1);
      await eventually(() => screen.getByRole("button", { name: "Jump" }));
    }

    it("Jump shows the agent the way the Agents pane does", async () => {
      await notifyThenReturnLate();
      expect(getPendingAttentionTarget()).toBe("sid-r");
      fireEvent.click(screen.getByRole("button", { name: "Jump" }));
      await eventually(() => expect(shown.showAgent).toHaveBeenCalledTimes(1));
      expect(shown.showAgent.mock.calls[0]).toEqual([
        expect.objectContaining({ handle: "reviewer", sessionId: "sid-r" }),
      ]);
    });

    it("an agent Show cannot open opens the Agents pane", async () => {
      shown.showAgent.mockResolvedValueOnce({ notShown: "Reviewer is still starting." });
      useViewerStore.getState().setAgentsPaneTeamHidden(TEAM, true);
      useViewerStore.getState().setWorkspacePanelCollapsed(true);
      await notifyThenReturnLate();
      fireEvent.click(screen.getByRole("button", { name: "Jump" }));
      await eventually(() => expect(useViewerStore.getState().workspacePanelCollapsed).toBe(false));
      // The pane opens with the team shown, and without filtering the pane to the one agent.
      expect(useViewerStore.getState().agentsPaneHiddenTeams).not.toContain(TEAM);
      expect(useViewerStore.getState().agentsListFilter).toBeNull();
    });
  });
});

describe("an agent's own signal_done outcome counts the same on the badge as in the Agents pane", () => {
  /** How many of the roster's rows the Agents pane counts as "Needs you". */
  const paneNeedsYou = () => {
    const rows = useAgentListProjectionStore.getState().sessions ?? [];
    const statusOf = getAgentStatusOf(rows);
    return rows.filter((r) => isNeedsYouStatus(statusOf(r))).length;
  };

  it.each<["needs-attention" | "failed", string]>([
    ["needs-attention", "Reviewer needs your attention"],
    ["failed", "Reviewer hit an error"],
  ])(
    "a tabless agent that signals %s: badge, notification and pane agree",
    async (status, title) => {
      setRoster(rosterAgent("reviewer", "sid-r", "Reviewer"));
      await start();
      setFocused(false);
      useManagedSessionStore.getState().markRunning("sid-r");
      useManagedSessionStore.getState().applyDoneStatus("sid-r", status, { summary: "Tests red." });
      projectSessionStatus("sid-r", status === "failed" ? "failed" : "dropped");
      await vi.advanceTimersByTimeAsync(BURST_MS * 2);
      expect(paneNeedsYou()).toBe(1);
      expect(lastBadge()).toBe(1);
      expect(titles()).toEqual([title]);
    },
  );

  it("an agent with a tab that signals needs-attention counts once, by its tab", async () => {
    setRoster(rosterAgent("tab-b", "sid-b", "Builder"));
    await start();
    setFocused(false);
    useManagedSessionStore.getState().applyDoneStatus("sid-b", "needs-attention");
    projectSessionStatus("sid-b", "dropped");
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(paneNeedsYou()).toBe(1);
    expect(lastBadge()).toBe(1);
    expect(titles()).toEqual(["W1 · Tab b needs your attention"]);
  });

  it("a successful signal_done needs no one, on either surface", async () => {
    setRoster(rosterAgent("reviewer", "sid-r", "Reviewer"));
    await start();
    setFocused(false);
    useManagedSessionStore.getState().applyDoneStatus("sid-r", "success");
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(paneNeedsYou()).toBe(0);
    expect(lastBadge()).toBeUndefined();
    expect(os.sendNotification).not.toHaveBeenCalled();
  });
});

describe("focused: the app shows it, the OS does not", () => {
  beforeEach(() => {
    render(<Toaster />);
  });

  it("an agent on another tab → the Jump pill, no OS notification", async () => {
    await start();
    setStatus("sid-c", "needs-permission");
    await settle();
    await eventually(() =>
      expect(screen.getByText("W2 · Tab c needs your permission")).toBeInTheDocument(),
    );
    expect(getPendingAttentionTarget()).toBe("sid-c");
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
  });

  it("the agent on the tab in view → nothing (the user can see it)", async () => {
    await start();
    setStatus("sid-a", "needs-permission");
    await settle();
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(getPendingAttentionTarget()).toBeNull();
    expect(os.sendNotification).not.toHaveBeenCalled();
    expect(lastBadge()).toBe(1);
  });

  it("a tabless agent → the Jump pill", async () => {
    setRoster(rosterAgent("reviewer", "sid-r", "Reviewer"));
    await start();
    ask(1, "sid-r");
    await settle();
    await eventually(() => expect(getPendingAttentionTarget()).toBe("sid-r"));
    expect(os.sendNotification).not.toHaveBeenCalled();
  });
});

describe("finished while you were away", () => {
  const hook = (sid: string, event: "UserPromptSubmit" | "Stop") =>
    useAgentStateStore.getState().recordSignal(sid, {
      tier: 1,
      signal: { kind: "claude-hook", event },
    });
  /** A turn on `sid` that runs `ms`, then stops. */
  async function turn(sid: string, ms: number) {
    hook(sid, "UserPromptSubmit");
    await settle();
    await vi.advanceTimersByTimeAsync(ms);
    hook(sid, "Stop");
    await settle();
  }
  const notes = () => os.sendNotification.mock.calls.map((c) => c[0]);

  it("a long turn finishing in the background notifies once, by its tab", async () => {
    await start();
    setFocused(false);
    await turn("sid-b", 45_000);
    await vi.advanceTimersByTimeAsync(BURST_MS);
    useViewerStore.setState({ aiStatusBySession: {} }); // an unrelated tick
    await vi.advanceTimersByTimeAsync(COOLDOWN_MS);
    expect(notes()).toEqual([{ title: "W1 · Tab b finished", body: "Worked for 45 seconds." }]);
    expect(lastBadge()).toBeUndefined();
  });

  it("carries the summary the agent left", async () => {
    await start();
    setFocused(false);
    hook("sid-b", "UserPromptSubmit");
    await settle();
    await vi.advanceTimersByTimeAsync(40_000);
    useManagedSessionStore.setState({ summaries: { "sid-b": "Opened PR #12 with the fix." } });
    hook("sid-b", "Stop");
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(notes()).toEqual([
      { title: "W1 · Tab b finished", body: "Opened PR #12 with the fix." },
    ]);
  });

  it("each turn notifies on its own", async () => {
    await start();
    setFocused(false);
    await turn("sid-b", 40_000);
    await vi.advanceTimersByTimeAsync(BURST_MS);
    await turn("sid-b", 40_000);
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(titles()).toEqual(["W1 · Tab b finished", "W1 · Tab b finished"]);
  });

  it("a tabless agent is named from the roster", async () => {
    setRoster(rosterAgent("reviewer", "sid-r", "Reviewer"));
    await start();
    setFocused(false);
    await turn("sid-r", 40_000);
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(titles()).toEqual(["Reviewer finished"]);
  });

  it.each<[string, () => Promise<void>]>([
    [
      "a short turn",
      async () => {
        setFocused(false);
        await turn("sid-b", 10_000);
      },
    ],
    [
      "the window is focused when it ends",
      async () => {
        setFocused(false);
        hook("sid-b", "UserPromptSubmit");
        await settle();
        await vi.advanceTimersByTimeAsync(40_000);
        setFocused(true);
        await settle();
        hook("sid-b", "Stop");
        await settle();
      },
    ],
    [
      "the user watched its tab during the turn",
      async () => {
        useViewerStore.getState().setActiveTerminalTab(TEAM, "sid-b", "w1");
        hook("sid-b", "UserPromptSubmit");
        await settle();
        setFocused(false);
        await vi.advanceTimersByTimeAsync(40_000);
        hook("sid-b", "Stop");
        await settle();
      },
    ],
    [
      "the setting is off",
      async () => {
        useViewerStore.getState().updateSettings({ notifyOnFinish: false });
        setFocused(false);
        await turn("sid-b", 40_000);
      },
    ],
  ])("no notification when %s", async (_label, run) => {
    await start();
    await run();
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
  });

  it.each<[string, Partial<ReturnType<typeof rosterAgent>> & Record<string, unknown>]>([
    ["running in the background", { backgrounded: true }],
    [
      "started by a routine",
      { provenance: { runId: "r1", storyId: "s1", routineKey: "acme/default/nightly" } },
    ],
  ])("an agent %s does not notify when it finishes", async (_label, extra) => {
    useAgentListProjectionStore.setState({
      sessions: [{ ...rosterAgent("ctl", "sid-r", "Controller"), ...extra }],
    });
    await start();
    setFocused(false);
    await turn("sid-r", 40_000);
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
  });

  describe("focused, on another tab", () => {
    beforeEach(() => {
      render(<Toaster />);
    });

    it("the Jump pill offers it, with no OS notification", async () => {
      await start();
      await turn("sid-c", 40_000);
      await eventually(() => expect(screen.getByText("W2 · Tab c finished")).toBeInTheDocument());
      expect(getPendingAttentionTarget()).toBe("sid-c");
      await vi.advanceTimersByTimeAsync(BURST_MS * 2);
      expect(os.sendNotification).not.toHaveBeenCalled();
    });

    it("a short turn is not offered", async () => {
      await start();
      await turn("sid-c", 10_000);
      await vi.advanceTimersByTimeAsync(BURST_MS * 2);
      expect(getPendingAttentionTarget()).toBeNull();
    });

    it("does not displace a pill for an agent that needs you", async () => {
      await start();
      hook("sid-c", "UserPromptSubmit");
      await settle();
      await vi.advanceTimersByTimeAsync(40_000);
      setStatus("sid-b", "needs-permission");
      await settle();
      await eventually(() => expect(getPendingAttentionTarget()).toBe("sid-b"));
      hook("sid-c", "Stop");
      await settle();
      await vi.advanceTimersByTimeAsync(BURST_MS);
      expect(getPendingAttentionTarget()).toBe("sid-b");
    });
  });

  it("a long command in a plain shell is not an agent finishing", async () => {
    await start();
    setFocused(false);
    setStatus("sid-b", "running");
    await settle();
    await vi.advanceTimersByTimeAsync(40_000);
    setStatus("sid-b", "done");
    await vi.advanceTimersByTimeAsync(BURST_MS * 2);
    expect(os.sendNotification).not.toHaveBeenCalled();
  });
});

describe("the notification sound", () => {
  let ua: ReturnType<typeof vi.spyOn>;
  beforeEach(() => {
    ua = vi
      .spyOn(navigator, "userAgent", "get")
      .mockReturnValue("Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)");
  });
  afterEach(() => ua.mockRestore());

  const sounds = () =>
    os.sendNotification.mock.calls.map((c) => (c[0] as { sound?: string }).sound);
  async function needsYouThenFinished() {
    await start();
    setFocused(false);
    setStatus("sid-a", "needs-permission");
    await vi.advanceTimersByTimeAsync(BURST_MS);
    useAgentStateStore.getState().recordSignal("sid-c", {
      tier: 1,
      signal: { kind: "claude-hook", event: "UserPromptSubmit" },
    });
    await settle();
    await vi.advanceTimersByTimeAsync(40_000);
    useAgentStateStore
      .getState()
      .recordSignal("sid-c", { tier: 1, signal: { kind: "claude-hook", event: "Stop" } });
    await vi.advanceTimersByTimeAsync(BURST_MS);
    expect(titles()).toEqual(["W1 · Tab a needs your permission", "W2 · Tab c finished"]);
  }

  it("is off by default", async () => {
    await needsYouThenFinished();
    expect(sounds()).toEqual([undefined, undefined]);
  });

  it("'Needs you' sounds for a needs-you notification, not a finished one", async () => {
    useViewerStore.getState().updateSettings({ notificationSound: "needs-you" });
    await needsYouThenFinished();
    expect(sounds()).toEqual(["Glass", undefined]);
  });

  it("'All' sounds for both", async () => {
    useViewerStore.getState().updateSettings({ notificationSound: "all" });
    await needsYouThenFinished();
    expect(sounds()).toEqual(["Glass", "Glass"]);
  });
});

describe("systemNotificationSound", () => {
  it("names the platform's own sound; Windows plays its default by itself", () => {
    expect(systemNotificationSound("Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)")).toBe("Glass");
    expect(systemNotificationSound("Mozilla/5.0 (X11; Linux x86_64)")).toBe("message-new-instant");
    expect(systemNotificationSound("Mozilla/5.0 (Windows NT 10.0; Win64; x64)")).toBeUndefined();
  });
});

it("notification entry prepares the named workspace before the team changes and records history", async () => {
  const other = "acme:ops";
  useOrgStore.setState({
    orgs: [{ slug: "acme", teams: [{ name: "default" }, { name: "ops" }] }] as ReturnType<
      typeof useOrgStore.getState
    >["orgs"],
    shownTeamsByOrgSlug: {},
    disabledOrgSlugs: [],
  });
  useViewerStore.setState({
    workspaces: { ...useViewerStore.getState().workspaces, [other]: [ws("o1"), ws("o2")] },
    activeWorkspace: { ...useViewerStore.getState().activeWorkspace, [other]: "o1" },
    contextHistory: { back: [], forward: [] },
  });
  seedTerminalTabs(other, "o2", ["attention-target"]);
  await settle();
  const frames: string[] = [];
  const stopObserve = useOrgStore.subscribe((next, prev) => {
    if (next.activeTeamName !== prev.activeTeamName)
      frames.push(useViewerStore.getState().activeWorkspace[other]);
  });
  try {
    expect(
      await routeToAttention({ sessionId: "attention-target", kind: "finished" }, {
        bringWindowForward: async () => {},
      } as Parameters<typeof routeToAttention>[1]),
    ).toBe(true);
    expect(frames).toEqual(["o2"]);
    expect(useViewerStore.getState().contextHistory.back).toEqual([
      { teamKey: TEAM, workspaceId: "w1" },
    ]);
  } finally {
    stopObserve();
  }
});

it("a notification for a same-team Hidden tab records the workspace before restoring it", async () => {
  useViewerStore.setState({ contextHistory: { back: [], forward: [] } });
  await hide("w2", "sid-h");
  expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe("w1");
  expect(
    await routeToAttention({ sessionId: "sid-h", kind: "finished" }, {
      bringWindowForward: async () => {},
    } as Parameters<typeof routeToAttention>[1]),
  ).toBe(true);
  expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe("w2");
  expect(useViewerStore.getState().contextHistory.back).toEqual([
    { teamKey: TEAM, workspaceId: "w1" },
  ]);
});

it("a refused notification does not restore or select a hidden background tab", async () => {
  const other = "acme:ops";
  useOrgStore.setState({
    orgs: [{ slug: "acme", teams: [{ name: "default" }, { name: "ops" }] }] as ReturnType<
      typeof useOrgStore.getState
    >["orgs"],
    shownTeamsByOrgSlug: { acme: ["default"] },
    disabledOrgSlugs: [],
  });
  useViewerStore.setState({
    workspaces: { ...useViewerStore.getState().workspaces, [other]: [ws("o1"), ws("o2")] },
    activeWorkspace: { ...useViewerStore.getState().activeWorkspace, [other]: "o1" },
    contextHistory: { back: [], forward: [] },
  });
  seedTerminalTabs(other, "o2", ["refused-attention", "other-tab"]);
  await settle();
  useViewerStore.getState().hideTerminalTab(other, "refused-attention", "o2");
  await settle();
  const restore = vi.spyOn(useViewerStore.getState(), "restoreHiddenTerminalTab");
  try {
    expect(
      await routeToAttention({ sessionId: "refused-attention", kind: "finished" }, {
        bringWindowForward: async () => {},
      } as Parameters<typeof routeToAttention>[1]),
    ).toBe(false);
    expect(restore).not.toHaveBeenCalled();
    expect(useViewerStore.getState().activeWorkspace[other]).toBe("o1");
    expect(
      selectHiddenTerminalTabs(useViewerStore.getState(), other, "o2").some(
        (tab) => tab.id === "refused-attention",
      ),
    ).toBe(true);
    expect(useViewerStore.getState().contextHistory.back).toEqual([]);
  } finally {
    restore.mockRestore();
  }
});

it.each([
  "team",
  "workspace",
  "tab",
  "activation",
  "editor-pointer",
  "overlay-keyboard",
  "same-current-panel",
  "right-pane",
] as const)(
  "a newer %s selection supersedes notification focus without a fallback Show",
  async (change) => {
    const stopCapture = installUserNavigationCapture(document);
    const frames: FrameRequestCallback[] = [];
    const raf = vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      frames.push(callback);
      return frames.length;
    });
    try {
      mountTile("w2", { question: true });
      const other = mountTile("w2", { question: true, teamKey: "acme:ops" });
      const chosen = document.createElement("button");
      document.body.appendChild(chosen);
      const pending = routeToAttention({ sessionId: "sid-c", kind: "question" }, {
        bringWindowForward: async () => {},
      } as Parameters<typeof routeToAttention>[1]);
      await Promise.resolve();
      expect(frames).toHaveLength(1);
      frames.shift()!(0);
      await Promise.resolve();
      if (change === "team") useOrgStore.setState({ activeTeamName: "ops" });
      else if (change === "workspace") switchContext({ teamKey: TEAM, workspaceId: "w1" }, "nav");
      else if (change === "tab") {
        const hook = renderHook(() => useTileTabActions(TEAM, "w2", () => {}));
        act(() => hook.result.current.onSelectTab("sid-h"));
        await settle();
        hook.unmount();
      } else if (change === "activation")
        activateAndFocus({ teamKey: TEAM, workspaceId: "w2", focus: "preserve-focus" });
      else if (change === "right-pane") useViewerStore.getState().toggleRightPane();
      else
        chosen.dispatchEvent(
          change === "overlay-keyboard"
            ? new KeyboardEvent("keydown", { key: "Tab", bubbles: true })
            : new MouseEvent("pointerdown", { bubbles: true }),
        );
      chosen.focus();
      frames.shift()!(0);
      expect(await pending).toBe(true);
      expect(document.activeElement).toBe(chosen);
      expect(document.activeElement).not.toBe(other.frame);
      expect(shown.showAgent).not.toHaveBeenCalled();
    } finally {
      stopCapture();
      raf.mockRestore();
    }
  },
);

it.each(["question", "finished"] as const)(
  "notification %s focus uses only the owning team's frame inside the CLI host",
  async (kind) => {
    mountTile("w2", { question: true, outsideCli: true });
    mountTile("w2", { question: true, teamKey: "acme:ops" });
    const { frame } = mountTile("w2", { question: true });
    expect(
      await routeToAttention({ sessionId: "sid-c", kind }, {
        bringWindowForward: async () => {},
      } as Parameters<typeof routeToAttention>[1]),
    ).toBe(true);
    expect(document.activeElement).toBe(
      kind === "question" ? frame.querySelector(`[${TILE_QUESTION_ATTR}]`) : frame,
    );
  },
);

it("a newer file open while the window is coming forward cancels notification entry", async () => {
  let forward!: () => void;
  const wait = new Promise<void>((resolve) => {
    forward = resolve;
  });
  const pending = routeToAttention({ sessionId: "sid-c", kind: "question" }, {
    bringWindowForward: () => wait,
  } as Parameters<typeof routeToAttention>[1]);
  useViewerStore.getState().openFile(TEAM, "", "/chosen.md");
  forward();
  expect(await pending).toBe(true);
  expect(view()).toEqual({ ws: "w1", tab: "sid-a" });
});

it("focus parking helpers preserve the non-DOM guard when no root is passed", () => {
  vi.stubGlobal("document", undefined);
  try {
    expect(focusTileFrame("w2")).toBe(false);
    expect(focusTileQuestionBanner("w2")).toBe(false);
  } finally {
    vi.unstubAllGlobals();
  }
});

it("a newer same-team activation during window-forward cancels notification entry", async () => {
  let forward!: () => void;
  const wait = new Promise<void>((resolve) => {
    forward = resolve;
  });
  const pending = routeToAttention({ sessionId: "sid-c", kind: "question" }, {
    bringWindowForward: () => wait,
  } as Parameters<typeof routeToAttention>[1]);
  activateAndFocus({ teamKey: TEAM, workspaceId: "w1", focus: "preserve-focus" });
  forward();
  expect(await pending).toBe(true);
  expect(view()).toEqual({ ws: "w1", tab: "sid-a" });
});

it("a slow tab-selection acknowledgement still parks notification focus safely", async () => {
  const { frame } = mountTile("w1", { question: true });
  // The notification names sid-b, but its host has not acknowledged selecting it yet.
  const select = vi
    .spyOn(useViewerStore.getState(), "setActiveTerminalTab")
    .mockImplementation(() => {});
  try {
    expect(
      await routeToAttention({ sessionId: "sid-b", kind: "question" }, {
        bringWindowForward: async () => {},
      } as Parameters<typeof routeToAttention>[1]),
    ).toBe(true);
    expect(select).toHaveBeenCalledWith(TEAM, "sid-b", "w1");
    expect(view().tab).toBe("sid-a");
    expect(document.activeElement).toBe(frame);
  } finally {
    select.mockRestore();
  }
});

it.each(["older-first", "newer-first"] as const)(
  "overlapping notification entries keep the newer destination when window replies are %s",
  async (order) => {
    let olderReady!: () => void;
    let newerReady!: () => void;
    const older = routeToAttention({ sessionId: "sid-b", kind: "finished" }, {
      bringWindowForward: () =>
        new Promise<void>((resolve) => {
          olderReady = resolve;
        }),
    } as Parameters<typeof routeToAttention>[1]);
    const newer = routeToAttention({ sessionId: "sid-c", kind: "finished" }, {
      bringWindowForward: () =>
        new Promise<void>((resolve) => {
          newerReady = resolve;
        }),
    } as Parameters<typeof routeToAttention>[1]);
    if (order === "older-first") {
      olderReady();
      expect(await older).toBe(true);
      expect(view()).toEqual({ ws: "w1", tab: "sid-a" });
      newerReady();
      expect(await newer).toBe(true);
    } else {
      newerReady();
      expect(await newer).toBe(true);
      olderReady();
      expect(await older).toBe(true);
    }
    expect(view()).toEqual({ ws: "w2", tab: "sid-c" });
  },
);
