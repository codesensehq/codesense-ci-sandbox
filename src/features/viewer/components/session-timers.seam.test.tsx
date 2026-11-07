// SPDX-License-Identifier: MIT
import type React from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@/test-utils";

// Seam: the real caption, timer chip, Timers and Remind popovers, the real
// session-timers store and its event watcher, the real routines and agents
// stores. Stubbed: the Tauri commands and event bus, the tab's agent lookup
// (the roster), the heavy repo resolver and the management-tab opener.

type Handler = (event: { payload: unknown }) => void;
const h = vi.hoisted(() => ({
  handlers: new Map<string, Handler>(),
  sessionTimers: vi.fn(),
  cancelSessionTimer: vi.fn(),
  remindSession: vi.fn(),
  wakeHistory: vi.fn(),
  agent: vi.fn(),
  openManagementTab: vi.fn(),
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }),
  cwd: vi.fn(),
  terminalFor: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((name: string, handler: Handler) => {
    h.handlers.set(name, handler);
    return Promise.resolve(() => h.handlers.delete(name));
  }),
  emit: vi.fn(),
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  sessionsRunningProcesses: vi.fn().mockResolvedValue([{ foreground: null, background: [] }]),
  sessionCwd: (sid: string) => h.cwd(sid),
  agentReadInbox: vi.fn().mockResolvedValue({ messages: [] }),
  sessionTimers: h.sessionTimers,
  cancelSessionTimer: h.cancelSessionTimer,
  remindSession: h.remindSession,
  wakeHistory: h.wakeHistory,
}));
vi.mock("@/features/viewer/hooks/use-tab-source-repo", () => ({
  useTabSourceRepo: () => ({ folder: null, repo: null }),
}));
vi.mock("@/features/viewer/hooks/use-tab-timer-agent", () => ({
  useTabTimerAgent: (sessionId: string | undefined) => h.agent(sessionId),
  useTabTimerAgents: () => new Map(),
}));
vi.mock("@/shared/terminal/terminal-manager", async (importOriginal) => {
  const real = await importOriginal<typeof import("@/shared/terminal/terminal-manager")>();
  return {
    ...real,
    terminalManager: { ...real.terminalManager, terminalFor: h.terminalFor },
  };
});
vi.mock("@/shared/lib/open-management-tab", () => ({
  openManagementTab: h.openManagementTab,
}));
vi.mock("sonner", () => ({ toast: h.toast }));
const machineActions = vi.hoisted(() => ({
  offerTerminalServerUpdate: vi.fn(),
}));
vi.mock("@/features/machines/lib/machine-actions", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/features/machines/lib/machine-actions")>()),
  ...machineActions,
}));

import { TerminalGridCaption } from "./terminal-grid-caption";
import { RemindPopover } from "./remind-popover";
import { TerminalTabBar } from "./terminal-tab-bar";
import { KeepTabTimers } from "./session-timers-loader";
import { resetWakeMarkersForTests } from "../lib/wake-marker";
import { WAKE_DELIVERED_EVENT, type WakeDeliveredEvent } from "@/shared/lib/wake-events";
import {
  WOKEN_CAPTION_MS,
  resetSessionTimersForTests,
  useSessionTimersStore,
} from "@/features/viewer/stores/session-timers-store";
import { useViewerStore, type TerminalTab } from "@/shared/stores/viewer-store";
import { useRoutinesStore } from "@/features/routines/stores/routines-store";
import { useAgentsStore } from "@/features/agents/stores/agents-store";
import type { SessionTimerRow } from "@/tauri/commands";

const MIN = 60_000;
const REMOTE_ORIGIN = { machineId: "codesense-test-1", member: "m", handle: "h" };
const BARB = { name: "Barb", agentKey: "acme/core/barb", saved: true, runtime: "claude" as const };
const UNNAMED = {
  name: "claude",
  agentKey: "builtin/claude",
  saved: false,
  runtime: "claude" as const,
};

function tabOf(patch: Partial<TerminalTab> = {}): TerminalTab {
  return { id: "tab-a", sessionId: "s-a", name: "barb", ...patch };
}

function timer(id: string, inMs: number, patch: Partial<SessionTimerRow> = {}): SessionTimerRow {
  return {
    timerId: id,
    name: id === "t1" ? "Check the build" : "Inbox nudge",
    prompt:
      id === "t1"
        ? "Check whether CI on #1973 finished and fix it if red"
        : "Read your inbox and answer anything addressed to you",
    nextAt: new Date(Date.now() + inMs).toISOString(),
    every: null,
    until: null,
    count: null,
    remaining: null,
    fires: 0,
    createdBy: "agent:Barb",
    machineId: "codesense-test-1",
    sessionId: "s-a",
    ...patch,
  };
}

const TWO = () => [
  timer("t1", 18 * MIN + 30_000),
  timer("t2", 42 * MIN, { every: "15m", createdBy: "you" }),
];

/** A tab's caption, with the app's loader keeping that tab's timers current. */
function caption(
  tab: TerminalTab,
  overrides: Partial<React.ComponentProps<typeof TerminalGridCaption>> = {},
) {
  return (
    <>
      <KeepTabTimers tab={tab} />
      {captionOnly(tab, overrides)}
    </>
  );
}

function captionOnly(
  tab: TerminalTab,
  overrides: Partial<React.ComponentProps<typeof TerminalGridCaption>> = {},
) {
  return (
    <TerminalGridCaption
      tab={tab}
      isActive
      background="#0d1117"
      foreground="#e6edf3"
      onFocusCell={() => undefined}
      onZoomCell={() => undefined}
      onClose={() => undefined}
      workspace={{ id: "ws", kind: "generic", cwd: "/w", status: "ready" } as never}
      teamKey="t1"
      {...overrides}
    />
  );
}

/** A terminal whose buffer holds `lines` (the cursor on the last). */
function fakeTerm(lines: string[]) {
  const buffer = {
    lines,
    get length() {
      return this.lines.length;
    },
    baseY: 0,
    get cursorY() {
      return this.lines.length - 1;
    },
    getLine(y: number) {
      const text = this.lines[y];
      return text === undefined ? undefined : { translateToString: () => text };
    },
  };
  return {
    buffer: { active: buffer },
    cols: 80,
    registerMarker: vi.fn((offset: number) => ({
      line: buffer.cursorY + offset,
      isDisposed: false,
      onDispose: vi.fn(),
    })),
    registerDecoration: vi.fn(() => ({ onRender: vi.fn() })),
    scrollToLine: vi.fn(),
  };
}

const chip = () => screen.getByTestId("terminal-grid-caption-timers-tab-a");

// Wall-clock text ("Goes in at 12:20 PM") depends on the local time of day,
// so freeze Date at a fixed mid-day instant; a run within 20 minutes of midnight must not differ.
const FROZEN_NOW = new Date(2026, 9, 5, 12, 0, 0);

beforeEach(() => {
  vi.useFakeTimers({ now: FROZEN_NOW, toFake: ["Date"] });
  resetSessionTimersForTests();
  h.handlers.clear();
  h.sessionTimers.mockReset().mockResolvedValue([]);
  h.cancelSessionTimer.mockReset().mockResolvedValue(true);
  h.remindSession.mockReset();
  h.wakeHistory.mockReset().mockResolvedValue([]);
  h.agent.mockReset().mockReturnValue(BARB);
  h.openManagementTab.mockReset();
  h.toast.mockReset();
  h.cwd.mockReset().mockResolvedValue("/Users/me/repos/codesense");
  h.terminalFor.mockReset().mockReturnValue(null);
  resetWakeMarkersForTests();
  useViewerStore.setState({ progressBySession: {} });
  useRoutinesStore.setState({ creating: false, formDirty: false, pendingLeave: null });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  resetSessionTimersForTests();
});

describe("AC-1: the timer chip and the Timers popover, remote and local", () => {
  it.each([
    ["remote", tabOf({ origin: REMOTE_ORIGIN }), "codesense-test-1", "on codesense-test-1"],
    ["local", tabOf(), undefined, "on This Mac"],
  ])(
    "a %s tab with 2 timers shows the next countdown, and the popover lists both with Cancel",
    async (_where, tab, machineId, where) => {
      h.sessionTimers.mockResolvedValue(TWO());
      render(caption(tab));
      await waitFor(() => expect(chip()).toBeInTheDocument());
      expect(h.sessionTimers).toHaveBeenCalledWith("s-a", machineId);
      expect(chip()).toHaveTextContent("18m");
      expect(chip()).toHaveTextContent("+1");
      expect(chip()).toHaveAccessibleName("Timers: 2, next in 18 minutes");
      expect(chip()).toHaveAttribute("aria-haspopup", "dialog");

      fireEvent.click(chip());
      const dialog = await screen.findByRole("dialog", { name: "Timers on this tab" });
      expect(chip()).toHaveAttribute("aria-expanded", "true");
      // Focus goes to the dialog itself, never to Cancel (it acts without a confirm).
      await waitFor(() => expect(dialog).toHaveFocus());
      expect(within(dialog).getByText(where)).toBeInTheDocument();
      const rows = within(dialog).getAllByTestId("session-timer-row");
      expect(rows).toHaveLength(2);
      expect(rows[0]).toHaveTextContent("Check the build");
      expect(rows[0]).toHaveTextContent("in 18m");
      expect(rows[0]).toHaveTextContent("Once · set by Barb");
      expect(rows[1]).toHaveTextContent("Every 15 min · set by you");
      // "Keep this…" is offered on every timer, one-shots included (decision 4).
      expect(within(dialog).getAllByTestId("timer-keep")).toHaveLength(2);

      const cancels = within(dialog).getAllByTestId("session-timer-cancel");
      expect(cancels).toHaveLength(2);
      fireEvent.click(cancels[0]);
      // Hidden at once; cancelled for real only when its Undo toast closes.
      await waitFor(() =>
        expect(within(dialog).getAllByTestId("session-timer-row")).toHaveLength(1),
      );
      expect(h.cancelSessionTimer).not.toHaveBeenCalled();
      expect(h.toast).toHaveBeenCalledWith(
        "Timer “Check the build” cancelled.",
        expect.objectContaining({
          duration: 5_000,
          action: expect.objectContaining({ label: "Undo" }),
        }),
      );
      const options = h.toast.mock.calls.at(-1)![1] as { onAutoClose: () => void };
      act(() => options.onAutoClose());
      await waitFor(() =>
        expect(h.cancelSessionTimer).toHaveBeenCalledWith("s-a", "t1", machineId),
      );
    },
  );

  it("Undo puts the timer back and nothing is cancelled (the mock's toast)", async () => {
    h.sessionTimers.mockResolvedValue(TWO());
    render(caption(tabOf({ origin: REMOTE_ORIGIN })));
    await waitFor(() => expect(chip()).toBeInTheDocument());
    fireEvent.click(chip());
    const dialog = await screen.findByRole("dialog", { name: "Timers on this tab" });
    fireEvent.click(within(dialog).getAllByTestId("session-timer-cancel")[0]);
    await waitFor(() => expect(within(dialog).getAllByTestId("session-timer-row")).toHaveLength(1));
    expect(chip()).not.toHaveTextContent("+1");
    const options = h.toast.mock.calls.at(-1)![1] as {
      action: { onClick: () => void };
      onDismiss: () => void;
    };
    act(() => options.action.onClick());
    // Sonner closes the toast after its action: that commit finds nothing to do.
    act(() => options.onDismiss());
    await waitFor(() => expect(within(dialog).getAllByTestId("session-timer-row")).toHaveLength(2));
    expect(h.cancelSessionTimer).not.toHaveBeenCalled();
  });

  it("a timer about to fire (under 6 s) is cancelled at once, with no Undo", async () => {
    h.sessionTimers.mockResolvedValue([timer("t1", 3_000), timer("t2", 42 * MIN)]);
    render(caption(tabOf()));
    await waitFor(() => expect(chip()).toBeInTheDocument());
    fireEvent.click(chip());
    const dialog = await screen.findByRole("dialog", { name: "Timers on this tab" });
    fireEvent.click(within(dialog).getAllByTestId("session-timer-cancel")[0]);
    await waitFor(() => expect(h.cancelSessionTimer).toHaveBeenCalledWith("s-a", "t1", undefined));
    expect(h.toast).toHaveBeenLastCalledWith("Timer “Check the build” cancelled.");
  });

  it("a background tab's timers are read without its caption on screen", async () => {
    h.sessionTimers.mockResolvedValue(TWO());
    render(<KeepTabTimers tab={tabOf({ origin: REMOTE_ORIGIN })} />);
    await waitFor(() =>
      expect(useSessionTimersStore.getState().bySession["codesense-test-1|s-a"]?.rows).toHaveLength(
        2,
      ),
    );
    expect(h.sessionTimers).toHaveBeenCalledWith("s-a", "codesense-test-1");
  });

  it("shows no chip when the tab has no timers", async () => {
    render(caption(tabOf()));
    await waitFor(() => expect(h.sessionTimers).toHaveBeenCalled());
    expect(screen.queryByTestId("terminal-grid-caption-timers-tab-a")).toBeNull();
  });

  it("the caption itself never reads timers (the app's loader does, for agent tabs only)", async () => {
    h.agent.mockReturnValue(null);
    render(captionOnly(tabOf()));
    await act(async () => {});
    expect(h.sessionTimers).not.toHaveBeenCalled();
  });

  it("timers die with the session: an ended tab drops its chip and its rows", async () => {
    h.sessionTimers.mockResolvedValue(TWO());
    const view = render(caption(tabOf({ origin: REMOTE_ORIGIN })));
    await waitFor(() => expect(chip()).toBeInTheDocument());
    view.rerender(caption(tabOf({ origin: REMOTE_ORIGIN, exit: { code: 0, signal: null } })));
    await waitFor(() =>
      expect(screen.queryByTestId("terminal-grid-caption-timers-tab-a")).toBeNull(),
    );
    expect(useSessionTimersStore.getState().bySession["codesense-test-1|s-a"]).toBeUndefined();
  });

  it("a reconnect re-reads the timers the machine holds", async () => {
    h.sessionTimers.mockResolvedValue(TWO());
    const view = render(caption(tabOf({ origin: REMOTE_ORIGIN })));
    await waitFor(() => expect(chip()).toBeInTheDocument());
    expect(h.sessionTimers).toHaveBeenCalledTimes(1);
    view.rerender(caption(tabOf({ origin: { ...REMOTE_ORIGIN, offlineSince: 1 } })));
    h.sessionTimers.mockResolvedValue([timer("t2", 42 * MIN, { every: "15m" })]);
    view.rerender(caption(tabOf({ origin: REMOTE_ORIGIN })));
    await waitFor(() => expect(h.sessionTimers).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(chip()).not.toHaveTextContent("+1"));
  });

  it("an offline machine keeps its last-known timers, dimmed, without Cancel", async () => {
    h.sessionTimers.mockResolvedValue(TWO());
    const view = render(caption(tabOf({ origin: REMOTE_ORIGIN })));
    await waitFor(() => expect(chip()).toBeInTheDocument());
    view.rerender(caption(tabOf({ origin: { ...REMOTE_ORIGIN, offlineSince: 1 } })));
    fireEvent.click(chip());
    const dialog = await screen.findByRole("dialog", { name: "Timers on this tab" });
    expect(dialog).toHaveTextContent(
      "codesense-test-1 is offline. Its timers keep firing there; you can see and change them once it's back.",
    );
    expect(within(dialog).getAllByTestId("session-timer-row")).toHaveLength(2);
    expect(within(dialog).queryByTestId("session-timer-cancel")).toBeNull();
  });

  it("an old terminal server says to update it", async () => {
    h.sessionTimers.mockRejectedValue(
      "Update the terminal server on codesense-test-1 to use wakes and timers.",
    );
    render(
      <RemindPopover
        open
        onOpenChange={() => undefined}
        target={{ sessionId: "s-a", machineId: "codesense-test-1" }}
        agentName="Barb"
        anchor={null}
        startOn="timers"
      />,
    );
    const dialog = await screen.findByRole("dialog", { name: "Timers on this tab" });
    await waitFor(() =>
      expect(dialog).toHaveTextContent(
        "Update the terminal server on codesense-test-1 to use timers.",
      ),
    );
    expect(within(dialog).getByRole("button", { name: "Update…" })).toBeInTheDocument();
    // SW-AGR-19: through the one update router, for that machine.
    fireEvent.click(within(dialog).getByRole("button", { name: "Update…" }));
    expect(machineActions.offerTerminalServerUpdate).toHaveBeenCalledWith("codesense-test-1");
  });

  it("SW-AGR-19: This Mac's old server offers This Mac's own update", async () => {
    machineActions.offerTerminalServerUpdate.mockClear();
    h.sessionTimers.mockRejectedValue(
      "Update the terminal server on This Mac to use wakes and timers.",
    );
    render(
      <RemindPopover
        open
        onOpenChange={() => undefined}
        target={{ sessionId: "s-a" }}
        agentName="Barb"
        anchor={null}
        startOn="timers"
      />,
    );
    const dialog = await screen.findByRole("dialog", { name: "Timers on this tab" });
    fireEvent.click(await within(dialog).findByRole("button", { name: "Update…" }));
    expect(machineActions.offerTerminalServerUpdate).toHaveBeenCalledWith("local");
  });

  it("the tab pill wears the alarm glyph and says how many timers it has", async () => {
    useSessionTimersStore.setState({
      bySession: {
        "local|s-a": { rows: TWO(), error: null, loading: false, oldServer: false },
      },
    });
    render(
      <TerminalTabBar
        tabs={[tabOf()]}
        activeTabId="tab-a"
        onSelectTab={vi.fn()}
        onCloseTab={vi.fn()}
        onNewShell={vi.fn()}
        onReorderTab={vi.fn()}
        onRenameTab={vi.fn()}
      />,
    );
    expect(screen.getByTestId("tab-tab-a-timers")).toBeInTheDocument();
    expect(screen.getByRole("tab")).toHaveAccessibleName(
      expect.stringContaining("2 timers, next in 18 minutes"),
    );
  });
});

describe("AC-2: Remind this agent…", () => {
  it("in 20 minutes sets a timer on the tab's machine, as the person", async () => {
    h.sessionTimers.mockResolvedValue(TWO());
    h.remindSession.mockResolvedValue({
      timerId: "t9",
      nextAt: new Date(Date.now() + 20 * MIN).toISOString(),
      name: "Check the build",
      machineId: "codesense-test-1",
      sessionId: "s-a",
    });
    render(caption(tabOf({ origin: REMOTE_ORIGIN })));
    await waitFor(() => expect(chip()).toBeInTheDocument());
    fireEvent.click(chip());
    const dialog = await screen.findByRole("dialog", { name: "Timers on this tab" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Remind this agent…" }));

    const remind = await screen.findByTestId("remind-popover");
    expect(within(remind).getByText("Remind Barb")).toBeInTheDocument();
    const message = within(remind).getByLabelText("Message");
    await waitFor(() => expect(message).toHaveFocus());
    expect(within(remind).getByLabelText("Amount")).toHaveValue("20");
    expect(within(remind).getByLabelText("Unit")).toHaveValue("minutes");

    fireEvent.click(within(remind).getByTestId("remind-submit"));
    expect(within(remind).getByText("Add a message.")).toBeInTheDocument();
    expect(h.remindSession).not.toHaveBeenCalled();

    fireEvent.change(message, { target: { value: "Check whether the build finished" } });
    expect(within(remind).getByText(/^Goes in at .* when Barb is idle\.$/)).toBeInTheDocument();
    fireEvent.click(within(remind).getByTestId("remind-submit"));
    await waitFor(() =>
      expect(h.remindSession).toHaveBeenCalledWith(
        "s-a",
        { in: "20m", prompt: "Check whether the build finished" },
        "codesense-test-1",
      ),
    );
    await waitFor(() =>
      expect(h.toast).toHaveBeenCalledWith(expect.stringMatching(/^Reminder set for .+\.$/)),
    );
  });

  it("an unnamed session's form is titled for this agent; a refusal shows inline", async () => {
    h.agent.mockReturnValue(UNNAMED);
    h.remindSession.mockRejectedValue("This tab already has 10 timers");
    render(
      <RemindPopover
        open
        onOpenChange={() => undefined}
        target={{ sessionId: "s-a" }}
        agentName={null}
        anchor={null}
      />,
    );
    const remind = await screen.findByTestId("remind-popover");
    expect(within(remind).getByText("Remind this agent")).toBeInTheDocument();
    fireEvent.change(within(remind).getByLabelText("Message"), { target: { value: "ping" } });
    fireEvent.click(within(remind).getByRole("tab", { name: "Every" }));
    fireEvent.change(within(remind).getByLabelText("Amount"), { target: { value: "15" } });
    fireEvent.click(within(remind).getByTestId("remind-submit"));
    await waitFor(() =>
      expect(h.remindSession).toHaveBeenCalledWith(
        "s-a",
        { in: "15m", every: "15m", prompt: "ping" },
        undefined,
      ),
    );
    expect(await within(remind).findByText("This tab already has 10 timers")).toBeInTheDocument();
  });
});

describe("AC-3: Woken by ‹name›", () => {
  function delivered(patch: Partial<WakeDeliveredEvent> = {}): WakeDeliveredEvent {
    return {
      machineId: "codesense-test-1",
      sessionId: "s-a",
      deliveryId: "d-1",
      marker: "Morning triage",
      caption: "Woken by Morning triage",
      origin: { kind: "routine", routineKey: "acme/core/barb/morning", runId: "run-1" },
      deliveredAtMs: Date.now(),
      source: "push",
      replayed: false,
      ...patch,
    };
  }

  it.each([
    ["remote", tabOf({ origin: REMOTE_ORIGIN }), "codesense-test-1"],
    ["local", tabOf(), "local"],
  ])("a delivery into a %s tab takes over the progress slot for 10 s", async (_w, tab, machine) => {
    vi.useFakeTimers({ now: FROZEN_NOW, shouldAdvanceTime: true });
    useViewerStore.setState({
      progressBySession: { "s-a": { note: "reviewing the inbox fix", settled: false } as never },
    });
    render(caption(tab));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(screen.getByTestId("terminal-grid-caption-progress-tab-a")).toBeInTheDocument();
    act(() =>
      h.handlers.get(WAKE_DELIVERED_EVENT)!({ payload: delivered({ machineId: machine }) }),
    );
    const woken = screen.getByTestId("terminal-grid-caption-woken-tab-a");
    expect(woken).toHaveTextContent("Woken by Morning triage");
    expect(screen.queryByTestId("terminal-grid-caption-progress-tab-a")).toBeNull();
    expect(screen.getByText("Barb woken by Morning triage")).toBeInTheDocument();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(WOKEN_CAPTION_MS);
    });
    // It fades out over 200 ms, then the progress note returns.
    expect(screen.getByTestId("terminal-grid-caption-woken-tab-a").className).toContain(
      "cs-woken-out",
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(200);
    });
    expect(screen.queryByTestId("terminal-grid-caption-woken-tab-a")).toBeNull();
    expect(screen.getByTestId("terminal-grid-caption-progress-tab-a")).toBeInTheDocument();
  });

  it("a delivery into another tab, or one on another machine, leaves this caption alone", async () => {
    render(caption(tabOf()));
    await act(async () => {});
    act(() => h.handlers.get(WAKE_DELIVERED_EVENT)!({ payload: delivered({ sessionId: "s-b" }) }));
    act(() =>
      h.handlers.get(WAKE_DELIVERED_EVENT)!({ payload: delivered({ machineId: "other" }) }),
    );
    expect(screen.queryByTestId("terminal-grid-caption-woken-tab-a")).toBeNull();
  });

  it("the wake history lists the tab's deliveries, newest first", async () => {
    h.sessionTimers.mockResolvedValue(TWO());
    h.wakeHistory.mockResolvedValue([
      {
        status: "delivered",
        deliveredAtMs: 1,
        deliveryId: "d1",
        marker: "Check CI",
        origin: { kind: "timer", timerId: "t0" },
        atMs: new Date(2026, 9, 5, 8, 15).getTime(),
      },
      {
        status: "unknown",
        detail: "x",
        deliveryId: "d2",
        marker: "Inbox sweep",
        origin: { kind: "routine", routineKey: "k", runId: "r" },
        atMs: new Date(2026, 9, 5, 8, 30).getTime(),
      },
    ]);
    render(caption(tabOf()));
    await waitFor(() => expect(chip()).toBeInTheDocument());
    fireEvent.click(chip());
    const dialog = await screen.findByRole("dialog", { name: "Timers on this tab" });
    const history = within(dialog).getByTestId("wake-history");
    fireEvent.click(within(history).getByText(/Wake history/));
    await waitFor(() => expect(within(history).getAllByRole("listitem")).toHaveLength(2));
    const items = within(history).getAllByRole("listitem");
    expect(items[0]).toHaveTextContent("Inbox sweep");
    expect(items[0]).toHaveTextContent(
      "May have been delivered. Check the terminal before re-sending.",
    );
    expect(items[1]).toHaveTextContent("Check CI");
    expect(items[1]).toHaveTextContent("Delivered");
  });
});

describe("AC-4: Keep this…", () => {
  async function keepFirst(tab: TerminalTab) {
    h.sessionTimers.mockResolvedValue(TWO());
    // A remote tab's directory is its workspace's on that machine.
    render(
      caption(tab, {
        workspace: {
          id: "ws",
          kind: "generic",
          cwd: "/Users/me/work/notify",
          status: "ready",
        } as never,
      }),
    );
    await waitFor(() => expect(chip()).toBeInTheDocument());
    fireEvent.click(chip());
    const dialog = await screen.findByRole("dialog", { name: "Timers on this tab" });
    fireEvent.click(within(dialog).getAllByTestId("timer-keep")[1]);
  }

  it("on an unnamed session saves an agent (runtime + cwd), then opens the routine form pre-filled", async () => {
    h.agent.mockReturnValue(UNNAMED);
    const saveAgent = vi.fn().mockResolvedValue({
      outcome: "saved",
      key: "me/claude-on-codesense-test-1",
      revision: 1,
      warnings: [],
    });
    useAgentsStore.setState({ saveAgent });
    await keepFirst(tabOf({ origin: REMOTE_ORIGIN }));

    const save = await screen.findByTestId("save-session-as-agent");
    const name = within(save).getByLabelText("Name");
    expect(name).toHaveValue("Claude on codesense-test-1");
    await waitFor(() =>
      expect(within(save).getByTestId("save-session-as-agent-from")).toHaveAttribute(
        "aria-label",
        "Runs Claude · was running in ~/work/notify · on codesense-test-1",
      ),
    );
    // A long path is cut in the middle; the full one is its title.
    expect(within(save).getByTestId("save-session-as-agent-cwd")).toHaveAttribute(
      "title",
      "~/work/notify",
    );
    fireEvent.click(within(save).getByRole("button", { name: "Save agent and continue" }));
    await waitFor(() => expect(saveAgent).toHaveBeenCalled());
    expect(saveAgent.mock.calls[0][0]).toMatchObject({
      name: "Claude on codesense-test-1",
      runtime: "claude",
      launch: "foreground",
    });
    await waitFor(() => expect(useRoutinesStore.getState().creating).toBe(true));
    expect(useRoutinesStore.getState().createSeed).toMatchObject({
      draft: {
        agent: "me/claude-on-codesense-test-1",
        name: "Inbox nudge",
        task: "Read your inbox and answer anything addressed to you",
        when: "every 15m",
      },
      mode: "wake",
      machineId: "codesense-test-1",
      timer: { sessionId: "s-a", machineId: "codesense-test-1", timerId: "t2" },
    });
    expect(h.openManagementTab).toHaveBeenCalled();
  });

  it("on a saved agent's tab goes straight to the routine form, no dialog", async () => {
    await keepFirst(tabOf());
    await waitFor(() => expect(useRoutinesStore.getState().creating).toBe(true));
    expect(screen.queryByTestId("save-session-as-agent")).toBeNull();
    expect(useRoutinesStore.getState().createSeed).toMatchObject({
      draft: { agent: "acme/core/barb", when: "every 15m", name: "Inbox nudge" },
      machineId: "local",
    });
  });
});

describe("r1: background tabs, offline Remind, the pill's wake words", () => {
  function deliveredTo(
    sessionId: string,
    patch: Partial<WakeDeliveredEvent> = {},
  ): WakeDeliveredEvent {
    return {
      machineId: "codesense-test-1",
      sessionId,
      deliveryId: "d-bg",
      marker: "Check the build (set by Barb)",
      caption: "Woken by Check the build (set by Barb)",
      origin: { kind: "timer", timerId: "t1" },
      deliveredAtMs: Date.now(),
      source: "push",
      replayed: false,
      ...patch,
    };
  }

  it("a delivery into an inactive tab marks its parked display at the delivery point; Wake history jumps to it", async () => {
    // Tab s-b sits behind the shown tab: no caption, only the loader and its parked display.
    const background = tabOf({ id: "tab-b", sessionId: "s-b", origin: REMOTE_ORIGIN });
    h.sessionTimers.mockResolvedValue([timer("t1", 18 * MIN, { sessionId: "s-b" })]);
    const term = fakeTerm([
      "$ claude",
      "earlier output",
      "> [timer: Check the build | fire 1]",
      "  Check whether CI on #1973 finished and fix it if red",
      "thinking…",
      "> ",
    ]);
    h.terminalFor.mockImplementation((machine: string, session: string) =>
      machine === "codesense-test-1" && session === "s-b" ? term : null,
    );
    render(<KeepTabTimers tab={background} />);
    await waitFor(() =>
      expect(useSessionTimersStore.getState().bySession["codesense-test-1|s-b"]?.rows).toHaveLength(
        1,
      ),
    );
    act(() => h.handlers.get(WAKE_DELIVERED_EVENT)!({ payload: deliveredTo("s-b") }));
    // The row above the header (line 1), not the cursor's row (line 5).
    expect(term.registerMarker).toHaveBeenCalledWith(1 - 5);

    h.wakeHistory.mockResolvedValue([
      {
        status: "delivered",
        deliveredAtMs: Date.now(),
        deliveryId: "d-bg",
        marker: "Check the build (set by Barb)",
        origin: { kind: "timer", timerId: "t1" },
        atMs: Date.now(),
      },
    ]);
    render(
      <RemindPopover
        open
        onOpenChange={() => undefined}
        target={{ sessionId: "s-b", machineId: "codesense-test-1" }}
        agentName="Barb"
        anchor={null}
        startOn="timers"
      />,
    );
    const dialog = await screen.findByRole("dialog", { name: "Timers on this tab" });
    const history = within(dialog).getByTestId("wake-history");
    fireEvent.click(within(history).getByText(/Wake history/));
    const item = await within(history).findByRole("button", { name: /Check the build/ });
    // The history names who set the timer.
    expect(item).toHaveTextContent("Check the build · timer set by Barb");
    fireEvent.click(item);
    expect(term.scrollToLine).toHaveBeenCalledWith(1);
  });

  it("Remind on an offline remote tab says so and sets nothing", async () => {
    render(
      <RemindPopover
        open
        onOpenChange={() => undefined}
        target={{ sessionId: "s-a", machineId: "codesense-test-1" }}
        agentName="Barb"
        offline
        anchor={null}
      />,
    );
    const remind = await screen.findByTestId("remind-popover");
    expect(remind).toHaveTextContent("codesense-test-1 is offline.");
    fireEvent.change(within(remind).getByLabelText("Message"), { target: { value: "ping" } });
    expect(within(remind).getByTestId("remind-submit")).toBeDisabled();
    expect(h.remindSession).not.toHaveBeenCalled();
  });

  it("Tab stays inside the Remind form", async () => {
    render(
      <RemindPopover
        open
        onOpenChange={() => undefined}
        target={{ sessionId: "s-a" }}
        agentName="Barb"
        anchor={null}
      />,
    );
    const remind = await screen.findByTestId("remind-popover");
    const submit = within(remind).getByTestId("remind-submit");
    submit.focus();
    fireEvent.keyDown(submit, { key: "Tab" });
    expect(remind.contains(document.activeElement)).toBe(true);
    expect(document.activeElement).not.toBe(submit);
  });

  it("a background tab's pill carries the wake words and announces them once per delivery", async () => {
    const tabs = [tabOf(), tabOf({ id: "tab-b", sessionId: "s-b", name: "notify" })];
    render(
      <>
        <KeepTabTimers tab={tabs[1]} />
        <TerminalTabBar
          tabs={tabs}
          activeTabId="tab-a"
          onSelectTab={vi.fn()}
          onCloseTab={vi.fn()}
          onNewShell={vi.fn()}
          onReorderTab={vi.fn()}
          onRenameTab={vi.fn()}
        />
      </>,
    );
    await act(async () => {});
    act(() =>
      h.handlers.get(WAKE_DELIVERED_EVENT)!({
        payload: deliveredTo("s-b", { machineId: "local", marker: "Morning triage" }),
      }),
    );
    expect(screen.getAllByRole("tab")[1]).toHaveAccessibleName(
      expect.stringContaining("woken by Morning triage"),
    );
    expect(screen.getByText("notify woken by Morning triage")).toBeInTheDocument();
  });
});
