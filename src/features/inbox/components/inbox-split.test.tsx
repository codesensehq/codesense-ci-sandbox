// SPDX-License-Identifier: MIT
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { AGENT_RUNTIMES } from "@/shared/lib/agent-runtimes";
import { useEffect, useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { act, fireEvent, render, renderHook, screen } from "@/test-utils";
import {
  closeAgentInboxRail,
  openAgentInboxRail,
  useAgentInboxRailStore,
} from "@/features/viewer/stores/agent-inbox-rail-store";

vi.mock("./inbox-rail", () => ({
  InboxRail: (p: { sessionId: string; style: React.CSSProperties }) => {
    const [selected, setSelected] = useState(false);
    return (
      <aside data-testid="inbox-rail" data-session-id={p.sessionId} style={p.style}>
        <button onClick={() => setSelected(true)}>
          {selected ? "Selected conversation" : "Select conversation"}
        </button>
      </aside>
    );
  },
}));

const tabIndex = vi.hoisted(() => ({
  running: {} as Record<string, Record<string, { id: string }[]>>,
}));
vi.mock("@/features/viewer/lib/cli-workspace/store/terminal-selectors", () => ({
  useRunningTerminalTabIndex: () => tabIndex.running,
  useMinimizedTerminalTabIndex: () => ({}),
}));

import { InboxSplit, useInboxTabCleanup } from "./inbox-split";
import {
  INBOX_RAIL_WIDTH_DEFAULT,
  INBOX_RAIL_WIDTH_MIN,
  inboxRailCssWidth,
} from "./inbox-splitter";

const mounts = vi.fn();
function Terminal() {
  useEffect(() => {
    mounts();
  }, []);
  return <div data-testid="terminal" />;
}

const TAB = { id: "tab-1", sessionId: "sess-1" };

beforeEach(() => {
  useAgentStateStore.setState({
    bySession: {},
    activityBySession: {},
    exitBySession: {},
    processAgentBySession: { "sess-1": "codex" },
  });
  mounts.mockClear();
  useAgentInboxRailStore.setState({ bySession: {}, widthByTab: {}, lensByTab: {} });
});

describe("InboxSplit", () => {
  it("hides during recovery without remounting the selected conversation", async () => {
    render(
      <div data-surface="terminal">
        <InboxSplit tab={TAB} teamKey="o:t">
          <Terminal />
        </InboxSplit>
      </div>,
    );
    act(() => openAgentInboxRail(TAB.sessionId));
    const rail = await screen.findByTestId("inbox-rail");
    expect(rail.closest("[data-surface]")).toHaveAttribute("data-surface", "canvas");
    expect(screen.getByTestId("terminal").closest("[data-surface]")).toHaveAttribute(
      "data-surface",
      "terminal",
    );
    fireEvent.click(screen.getByText("Select conversation"));
    act(() => useAgentStateStore.getState().activityUnavailable([TAB.sessionId], "recovering"));
    expect(rail).not.toBeVisible();
    act(() =>
      useAgentStateStore.getState().recordActivity(
        {
          session: TAB.sessionId,
          identity: {
            hostBootId: "split-host",
            sessionIncarnation: "split-shell",
            invocationId: crypto.randomUUID(),
          },
          revision: 1,
          turnGeneration: 0,
          provider: "codex",
          state: "idle",
          observed: true,
          limited: false,
          exit: null,
          asks: [],
        },
        0,
      ),
    );
    expect(screen.getByTestId("inbox-rail")).toBe(rail);
    expect(rail).toBeVisible();
    expect(screen.getByText("Selected conversation")).toBeVisible();
    expect(mounts).toHaveBeenCalledTimes(1);
  });

  it("closes on a real provider exit and only reopens after a relaunch", async () => {
    render(
      <InboxSplit tab={TAB} teamKey="o:t">
        <Terminal />
      </InboxSplit>,
    );
    act(() => openAgentInboxRail(TAB.sessionId));
    await screen.findByTestId("inbox-rail");
    act(() => {
      useAgentStateStore.getState().setProcessAgent(TAB.sessionId, null);
      useAgentStateStore.getState().recordSignal(TAB.sessionId, {
        tier: 1,
        signal: { kind: "claude-hook", event: "SessionEnd" },
      });
    });
    expect(screen.queryByTestId("inbox-rail")).toBeNull();
    expect(useAgentInboxRailStore.getState().bySession[TAB.sessionId]).toBeUndefined();
    act(() => openAgentInboxRail(TAB.sessionId));
    expect(screen.queryByTestId("inbox-rail")).toBeNull();
    act(() => {
      useAgentStateStore.getState().setProcessAgent(TAB.sessionId, "codex");
      openAgentInboxRail(TAB.sessionId);
    });
    await screen.findByTestId("inbox-rail");
    expect(mounts).toHaveBeenCalledTimes(1);
  });

  it.each(AGENT_RUNTIMES)(
    "docks the %s inbox beside the terminal without remounting the terminal",
    async (cli) => {
      useAgentStateStore.getState().setProcessAgent(TAB.sessionId, cli);
      render(
        <InboxSplit tab={TAB} teamKey="o:t">
          <Terminal />
        </InboxSplit>,
      );
      expect(screen.queryByTestId("inbox-rail")).toBeNull();
      act(() => openAgentInboxRail("sess-1"));
      const rail = await screen.findByTestId("inbox-rail");
      expect(rail).toBeVisible();
      expect(rail).toHaveAttribute("data-session-id", "sess-1");
      expect(screen.getByTestId("inbox-splitter")).toBeInTheDocument();
      act(() => closeAgentInboxRail("sess-1"));
      expect(screen.queryByTestId("inbox-rail")).toBeNull();
      expect(mounts).toHaveBeenCalledTimes(1);
    },
  );

  it("holds the rail inside the split's bounds", () => {
    expect(inboxRailCssWidth(INBOX_RAIL_WIDTH_DEFAULT)).toEqual({
      width: "min(520px, 72%)",
      minWidth: "min(360px, 72%)",
    });
  });

  it("resizes from the keyboard within its bounds and resets on Enter", async () => {
    act(() => openAgentInboxRail("sess-1"));
    render(
      <InboxSplit tab={TAB} teamKey="o:t">
        <Terminal />
      </InboxSplit>,
    );
    const handle = await screen.findByTestId("inbox-splitter");
    fireEvent.keyDown(handle, { key: "ArrowLeft" });
    const widened = useAgentInboxRailStore.getState().widthByTab["tab-1"]!;
    expect(widened).toBeGreaterThanOrEqual(INBOX_RAIL_WIDTH_MIN);
    fireEvent.keyDown(handle, { key: "Home" });
    expect(useAgentInboxRailStore.getState().widthByTab["tab-1"]).toBe(INBOX_RAIL_WIDTH_MIN);
    fireEvent.keyDown(handle, { key: "Enter" });
    // happy-dom measures the split as 0px wide, so the clamp floors at the minimum.
    expect(useAgentInboxRailStore.getState().widthByTab["tab-1"]).toBe(INBOX_RAIL_WIDTH_MIN);
  });

  it("ends a drag when pointer capture is lost (released outside the window)", async () => {
    act(() => openAgentInboxRail("sess-1"));
    render(
      <InboxSplit tab={TAB} teamKey="o:t">
        <Terminal />
      </InboxSplit>,
    );
    const handle = await screen.findByTestId("inbox-splitter");
    fireEvent.pointerDown(handle, { button: 0, pointerId: 7, clientX: 500 });
    expect(handle).toHaveAttribute("data-dragging", "true");
    act(() => {
      const lost = new Event("lostpointercapture", { bubbles: true });
      Object.assign(lost, { pointerId: 7 });
      window.dispatchEvent(lost);
    });
    expect(handle).toHaveAttribute("data-dragging", "false");
    expect(useAgentInboxRailStore.getState().widthByTab["tab-1"]).toBe(INBOX_RAIL_WIDTH_MIN);
  });
});

describe("useInboxTabCleanup", () => {
  it("forgets a closed tab's width and lens, not those of a tab merely Hidden or moved", () => {
    useAgentInboxRailStore.setState({
      widthByTab: { a: 400, b: 500 },
      lensByTab: { a: "spool", b: "wire" },
    });
    const a = { id: "a" };
    const b = { id: "b" };
    tabIndex.running = { team: { ws: [a, b] } };
    const { rerender } = renderHook(({ tabs }) => useInboxTabCleanup(tabs), {
      initialProps: { tabs: [a, b] },
    });
    // `a` is Hidden: off the tile's strip, still running.
    tabIndex.running = { team: { ws: [b, a] } };
    rerender({ tabs: [b] });
    expect(useAgentInboxRailStore.getState().lensByTab.a).toBe("spool");
    // `a` closes for good; `b` closes too.
    tabIndex.running = { team: { ws: [] } };
    rerender({ tabs: [] });
    expect(useAgentInboxRailStore.getState().lensByTab).toEqual({});
    expect(useAgentInboxRailStore.getState().widthByTab).toEqual({});
  });
});
