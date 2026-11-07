// SPDX-License-Identifier: MIT
import { projectSessionStatus, resetSessionProjection } from "@/test/session-projection-fixture";
/**
 * Seam test for the tab strip's words and semantics, through the real stores
 * (the terminal layout the host announces, the agent-state records, seen) and
 * the real tile chrome: every tab is a `tab` in a `tablist`, named by its name
 * and then its state in words (no ticking age — the dot's tooltip carries the
 * age, which keeps itself current on one shared clock); a crash names its
 * exit once; a finished turn reads Idle, and the urgent states never clear on
 * a look. Arrow keys rove the strip, Delete
 * closes a tab, and neither takes over the tab verbs or the global chords.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@/test-utils";
import userEvent from "@testing-library/user-event";

const running = vi.hoisted(() => new Set<string>());
vi.mock("@/tauri/commands", () => ({
  // Each team draws its agents with its own faces.
  avatarPacksListForTeam: vi.fn().mockResolvedValue({ packs: [], warnings: [] }),
  sessionsRunningProcesses: vi.fn(async (ids: string[]) =>
    ids.map((id) => ({ foreground: running.has(id) ? "claude" : null, background: [] })),
  ),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  killPty: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));
vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";
import { TerminalTileChrome } from "./terminal-tile-chrome";
import { useMarkActiveSessionSeen } from "../hooks/use-mark-active-session-seen";
import {
  resetDotSinceForTests,
  useScopeMark,
} from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import { jumpToTerminalTab, renameFocusedTab } from "../lib/terminal-tab-keys";
import {
  resetPrunedAgentSessionsForTests,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  selectActiveTerminalTab,
  selectWorkspaceTabs,
  useViewerStore,
  type Workspace,
} from "@/shared/stores/viewer-store";

const TEAM = "acme:core";
const WS: Workspace = { id: "ws", kind: "story", cwd: "/ws", status: "ready" };
let KEY = "";

/** The tile as the app mounts it: tabs and focus from the layout, seen marked
 *  by the app's own hook, a pill click selecting through the host. */
function Tile({
  onCloseTab = () => {},
  panedTabIds,
}: {
  onCloseTab?: (tabId: string) => void;
  panedTabIds?: string[];
}) {
  useMarkActiveSessionSeen();
  const tabs = useViewerStore((s) => selectWorkspaceTabs(s, TEAM, WS.id));
  const active = useViewerStore((s) => selectActiveTerminalTab(s, TEAM, WS.id));
  const noop = () => {};
  return (
    <TerminalTileChrome
      workspace={WS}
      isFocused
      tabs={tabs}
      activeTabId={active}
      onSelectTab={(tabId) => {
        const tab = tabs.find((t) => t.id === tabId);
        if (tab) terminalHost().setActive(KEY, tab.sessionId);
      }}
      onCloseTab={onCloseTab}
      panedTabIds={panedTabIds}
      onHideTab={noop}
      onEndTab={noop}
      onEndOtherTabs={noop}
      onEndAllTabs={noop}
      onReorderTab={noop}
      onRenameTab={noop}
      onFocusTile={noop}
      onNewShell={noop}
    />
  );
}

const pill = (name: string) => screen.getByRole("tab", { name: new RegExp(`^${name}\\b`) });
const dotOf = (name: string) => within(pill(name)).getByRole("img");

const crash = (session: string, code: number) => {
  act(() => {
    useAgentStateStore.getState().recordSignal(session, {
      tier: 1,
      signal: { kind: "claude-hook", event: "UserPromptSubmit" },
    });
    useAgentStateStore.getState().recordCommandEnd(session, code);
    projectSessionStatus(session, "crashed", {
      handle: session.slice(2),
      statusSinceMs: Date.now(),
    });
  });
};
const wait = (ms: number) => act(() => vi.advanceTimersByTime(ms));

installTerminalLayoutHarness();

let windowFocused = true;
beforeEach(() => {
  resetSessionProjection();
  running.clear();
  windowFocused = true;
  vi.spyOn(document, "hasFocus").mockImplementation(() => windowFocused);
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "core" });
  resetDotSinceForTests();
  resetPrunedAgentSessionsForTests();
  useAgentStateStore.setState({ bySession: {}, exitBySession: {}, stateSince: {}, seenAt: {} });
  useViewerStore.setState({
    activeWorkspace: { [TEAM]: WS.id },
    workspaces: { [TEAM]: [WS] },
    terminalVisible: true,
    softClosedWorkspaces: {},
    aiStatusBySession: {},
  });
  KEY = seedTerminalTabs(
    TEAM,
    WS.id,
    [
      { session: "s-build", name: "build" },
      { session: "s-review", name: "review" },
      { session: "s-docs", name: "docs" },
    ],
    { active: "s-build" },
  );
});
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("each tab says its state in words, with its age", () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    vi.setSystemTime(1_000_000);
  });

  it("the strip is a tablist of tabs, the active one selected, each named by its name then its state", () => {
    render(<Tile />);
    act(() => {
      projectSessionStatus("s-review", "paused", { handle: "review", statusSinceMs: Date.now() });
    });
    const list = screen.getByRole("tablist", { name: "Terminal tabs" });
    const tabs = within(list).getAllByRole("tab");
    expect(tabs.map((t) => t.getAttribute("aria-selected"))).toEqual(["true", "false", "false"]);
    expect(pill("build")).toHaveAccessibleName("build");
    expect(pill("review")).toHaveAccessibleName("review Needs approval");
    // The dot itself is an image with the words and the state's age, shown
    // as its tooltip.
    expect(dotOf("review")).toHaveAccessibleName("Needs approval · 0s");
  });

  it("the dot's age keeps itself current; the tab's name never ticks", () => {
    render(<Tile />);
    act(() => {
      projectSessionStatus("s-review", "paused", { handle: "review", statusSinceMs: Date.now() });
    });
    wait(30_000);
    expect(dotOf("review")).toHaveAccessibleName("Needs approval · 30s");
    wait(90_000);
    expect(dotOf("review")).toHaveAccessibleName("Needs approval · 2m");
    wait(2 * 60 * 60_000);
    expect(dotOf("review")).toHaveAccessibleName("Needs approval · 2h");
    expect(pill("review")).toHaveAccessibleName("review Needs approval");
  });

  it("a tab in the grid is named like any other tab", () => {
    const build = () => selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS.id)[0]!.id;
    render(<Tile panedTabIds={[build()]} />);
    act(() => {
      projectSessionStatus("s-build", "paused", { handle: "build", statusSinceMs: Date.now() });
    });
    expect(pill("build")).toHaveAccessibleName("build Needs approval");
    expect(screen.queryByRole("img", { name: "In grid" })).toBeNull();
  });

  it("a crash names how it ended, and its age restarts with each new state", () => {
    render(<Tile />);
    act(() => {
      projectSessionStatus("s-docs", "thinking", { handle: "docs", statusSinceMs: Date.now() });
    });
    wait(45_000);
    expect(dotOf("docs")).toHaveAccessibleName("Thinking · 45s");
    crash("s-docs", 137);
    expect(dotOf("docs")).toHaveAccessibleName("Crashed · exit 137 · 0s");
    wait(5 * 60_000);
    expect(dotOf("docs")).toHaveAccessibleName("Crashed · exit 137 · 5m");
    expect(pill("docs")).toHaveAccessibleName("docs Crashed, exit 137");
  });

  it("an ended tab names how it ended once, in the ended bar's words", () => {
    render(<Tile />);
    crash("s-docs", 1);
    act(() => terminalHost().exit("s-docs", 1));
    expect(pill("docs")).toHaveAccessibleName("docs Crashed, exit 1");
    expect(dotOf("docs")).toHaveAccessibleName("Crashed · exit 1");
    // A plain shell that exits by itself ended: the square says how, once.
    act(() => terminalHost().exit("s-review", 0));
    expect(dotOf("review")).toHaveAttribute("data-mark", "ended");
    expect(dotOf("review")).toHaveAccessibleName("Ended · exit 0");
    expect(pill("review")).toHaveAccessibleName("review Ended, exit 0");
  });
});

describe("a finished turn is Idle, looked at or not", () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    vi.setSystemTime(1_000_000);
  });

  it("a finished turn in another tab reads Idle at once; looking at it changes nothing", () => {
    render(<Tile />);
    act(() => {
      projectSessionStatus("s-review", "idle", { handle: "review", statusSinceMs: Date.now() });
    });
    wait(14_000);
    expect(dotOf("review")).toHaveAccessibleName("Ready · 14s");
    expect(dotOf("review")).toHaveAttribute("data-mark", "ready");
    expect(dotOf("review").querySelector(".cs-mark-spin, .cs-mark-halo")).toBeNull();

    fireEvent.click(pill("review"));
    expect(pill("review")).toHaveAttribute("aria-selected", "true");
    expect(dotOf("review")).toHaveAccessibleName("Ready · 14s");
    expect(dotOf("review")).toHaveAttribute("data-mark", "ready");
  });

  it("a turn that finishes in the tab being watched reads Ready", () => {
    render(<Tile />);
    act(() => {
      projectSessionStatus("s-build", "idle", { handle: "build", statusSinceMs: Date.now() });
    });
    expect(dotOf("build")).toHaveAccessibleName("Ready · 0s");
    expect(dotOf("build")).toHaveAttribute("data-mark", "ready");
  });

  it("an unfocused window's finished turn reads Ready too", () => {
    windowFocused = false;
    render(<Tile />);
    act(() => {
      projectSessionStatus("s-build", "idle", { handle: "build", statusSinceMs: Date.now() });
    });
    expect(dotOf("build")).toHaveAccessibleName("Ready · 0s");
  });

  it("the urgent states never clear on a look: Needs approval keeps its halo, a crash stays red", () => {
    render(<Tile />);
    act(() => {
      projectSessionStatus("s-review", "paused", { handle: "review", statusSinceMs: Date.now() });
    });
    crash("s-docs", 137);
    fireEvent.click(pill("review"));
    expect(dotOf("review")).toHaveAccessibleName("Needs approval · 0s");
    expect(dotOf("review").querySelector(".cs-mark-halo")).not.toBeNull();

    fireEvent.click(pill("docs"));
    expect(dotOf("docs")).toHaveAccessibleName("Crashed · exit 137 · 0s");
    expect(dotOf("docs")).toHaveAttribute("data-mark", "crashed");
  });
});

describe("the strip is one Tab stop that arrow keys rove", () => {
  it("arrows move focus between pills (wrapping), Home / End jump, Enter selects", async () => {
    const user = userEvent.setup();
    render(<Tile />);
    const stops = () => screen.getAllByRole("tab").map((t) => t.getAttribute("tabindex") === "0");
    // One stop, on the active tab.
    expect(stops()).toEqual([true, false, false]);

    act(() => pill("build").focus());
    await user.keyboard("{ArrowRight}");
    expect(pill("review")).toHaveFocus();
    expect(stops()).toEqual([false, true, false]);
    // Moving focus does not select.
    expect(pill("build")).toHaveAttribute("aria-selected", "true");

    await user.keyboard("{End}");
    expect(pill("docs")).toHaveFocus();
    await user.keyboard("{ArrowRight}");
    expect(pill("build")).toHaveFocus();
    await user.keyboard("{ArrowLeft}");
    expect(pill("docs")).toHaveFocus();
    await user.keyboard("{Home}");
    expect(pill("build")).toHaveFocus();

    await user.keyboard("{ArrowRight}{Enter}");
    expect(pill("review")).toHaveAttribute("aria-selected", "true");
    expect(pill("build")).toHaveAttribute("aria-selected", "false");
  });

  it("the tablist holds only tabs: the × is the mouse's, out of the tab order and hidden", () => {
    render(<Tile />);
    const list = screen.getByRole("tablist");
    const focusable = [...list.querySelectorAll<HTMLElement>("button, input, [tabindex]")];
    const exposed = focusable.filter((el) => !el.closest("[aria-hidden='true']"));
    expect(exposed.map((el) => el.getAttribute("role"))).toEqual(["tab", "tab", "tab"]);
    const closes = focusable.filter((el) => el.getAttribute("role") !== "tab");

    expect(closes).toHaveLength(3);
    for (const close of closes) {
      expect(close).toHaveAttribute("tabindex", "-1");
      expect(close).toHaveAttribute("aria-hidden", "true");
    }
  });

  it("leaving the strip hands the stop back to the active tab (Shift+Tab re-entry)", async () => {
    const user = userEvent.setup();
    render(
      <>
        <Tile />
        <button>after</button>
      </>,
    );
    const stops = () => screen.getAllByRole("tab").map((t) => t.getAttribute("tabindex") === "0");
    act(() => pill("build").focus());
    await user.keyboard("{ArrowRight}");
    expect(stops()).toEqual([false, true, false]);
    act(() => screen.getByRole("button", { name: "after" }).focus());
    expect(stops()).toEqual([true, false, false]);
    // Shift+Tab back past the tile's controls: the first tab reached is the active one.
    for (let i = 0; i < 10 && document.activeElement?.getAttribute("role") !== "tab"; i++) {
      await user.tab({ shift: true });
    }
    expect(pill("build")).toHaveFocus();
  });

  it("chords are not the strip's: ⌘⌥← / ⌃→ / ⇧→ pass through to the global shortcuts", () => {
    render(<Tile />);
    const reached: string[] = [];
    const listener = (e: KeyboardEvent) => reached.push(`${e.key}:${e.defaultPrevented}`);
    window.addEventListener("keydown", listener);
    try {
      act(() => pill("build").focus());
      for (const mods of [
        { metaKey: true, altKey: true },
        { ctrlKey: true },
        { shiftKey: true },
        { altKey: true },
      ]) {
        fireEvent.keyDown(pill("build"), { key: "ArrowRight", ...mods });
        fireEvent.keyDown(pill("build"), { key: "Delete", ...mods });
      }
      expect(pill("build")).toHaveFocus();
      expect(reached).toEqual(
        Array(8)
          .fill(null)
          .map((_, i) => (i % 2 ? "Delete:false" : "ArrowRight:false")),
      );
      // …while the bare key is the strip's own.
      fireEvent.keyDown(pill("build"), { key: "ArrowRight" });
      expect(pill("review")).toHaveFocus();
      expect(reached).toHaveLength(8);
    } finally {
      window.removeEventListener("keydown", listener);
    }
  });

  it("F2 still renames the pill that has focus, and jump-to-tab-N still selects", async () => {
    const user = userEvent.setup();
    render(<Tile />);
    act(() => pill("build").focus());
    await user.keyboard("{ArrowRight}{ArrowRight}");
    expect(pill("docs")).toHaveFocus();
    act(() => void renameFocusedTab());
    expect(await screen.findByRole("textbox", { name: "Tab name" })).toHaveValue("docs");

    expect(jumpToTerminalTab(TEAM, 2)).toBe(true);
    await waitFor(() => expect(pill("review")).toHaveAttribute("aria-selected", "true"));
  });

  it("Delete closes the focused tab with its close verb, then focus moves to its neighbour", async () => {
    const user = userEvent.setup();
    const onCloseTab = vi.fn((tabId: string) => {
      const tab = selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS.id).find(
        (t) => t.id === tabId,
      );
      if (tab) terminalHost().closeTab(tab.sessionId, "kill");
    });
    render(<Tile onCloseTab={onCloseTab} />);
    const review = () => selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS.id)[1]!.id;
    const reviewId = review();
    act(() => pill("build").focus());
    await user.keyboard("{ArrowRight}{Delete}");
    expect(onCloseTab).toHaveBeenCalledWith(reviewId);
    await waitFor(() => expect(screen.getAllByRole("tab")).toHaveLength(2));
    expect(pill("docs")).toHaveFocus();
  });

  it("Backspace closes a tab on macOS only", async () => {
    const user = userEvent.setup();
    const onCloseTab = vi.fn();
    const platform = vi.spyOn(navigator, "platform", "get");
    render(<Tile onCloseTab={onCloseTab} />);
    act(() => pill("build").focus());
    platform.mockReturnValue("Linux x86_64");
    await user.keyboard("{Backspace}");
    expect(onCloseTab).not.toHaveBeenCalled();
    platform.mockReturnValue("MacIntel");
    await user.keyboard("{Backspace}");
    expect(onCloseTab).toHaveBeenCalledTimes(1);
  });
});

describe("one shared clock for every age", () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    vi.setSystemTime(1_000_000);
  });

  it("many aging dots keep one timer, and none once they unmount", () => {
    const { unmount } = render(<Tile />);
    act(() => {
      projectSessionStatus("s-review", "paused", { handle: "review", statusSinceMs: Date.now() });
    });
    wait(10_000);
    crash("s-docs", 137);
    act(() => {
      projectSessionStatus("s-build", "idle", { handle: "build", statusSinceMs: Date.now() });
    });
    // Three dots, each labelled twice (tab and words), on one timer.
    expect(vi.getTimerCount()).toBe(1);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("a dot re-renders when its phrase changes, not on every tick", () => {
    let renders = 0;
    function Probe() {
      renders++;
      return <span>{useScopeMark({ sessionIds: ["s-review"] }).label}</span>;
    }
    render(
      <>
        <Tile />
        <Probe />
      </>,
    );
    act(() => {
      projectSessionStatus("s-review", "paused", { handle: "review", statusSinceMs: Date.now() });
    });
    // A younger state elsewhere ticks the clock each second…
    wait(90_000);
    act(() => {
      projectSessionStatus("s-docs", "thinking", { handle: "docs", statusSinceMs: Date.now() });
    });
    const before = renders;
    // …but a phrase in minutes changes only once in the next 59s.
    wait(59_000);
    expect(screen.getByText(/^Needs approval · \dm$/)).toBeInTheDocument();
    expect(renders - before).toBeLessThanOrEqual(1);
  });
});

describe("closing from the keyboard is discoverable and safe", () => {
  let platform: ReturnType<typeof vi.spyOn>;
  beforeEach(() => {
    platform = vi.spyOn(navigator, "platform", "get").mockReturnValue("Linux x86_64");
  });

  it("the × tooltip names its verb and the key that does it", async () => {
    const user = userEvent.setup();
    render(<Tile />);
    const close = within(screen.getByRole("tablist"))
      .getAllByRole("button", { hidden: true })
      .filter((b) => b.getAttribute("aria-hidden") === "true")[0]!;
    // Mouse-only: no accessible name of its own.
    expect(close).not.toHaveAttribute("aria-label");
    await user.hover(close);
    expect(await screen.findByRole("tooltip")).toHaveTextContent(
      /^(Hide · keeps running|End) \(Del\)$/,
    );
  });

  it("⌫ is the key shown on macOS", async () => {
    platform.mockReturnValue("MacIntel");
    const user = userEvent.setup();
    render(<Tile />);
    const close = within(screen.getByRole("tablist"))
      .getAllByRole("button", { hidden: true })
      .filter((b) => b.getAttribute("aria-hidden") === "true")[0]!;
    await user.hover(close);
    expect(await screen.findByRole("tooltip")).toHaveTextContent(/\(⌫\)$/);
  });

  it("Shift+F10 on a pill opens its menu, where the verb Delete takes shows the key", async () => {
    const user = userEvent.setup();
    running.add("s-review");
    render(<Tile />);
    act(() => pill("build").focus());
    await user.keyboard("{Shift>}{F10}{/Shift}");
    const menu = await screen.findByRole("menu");
    const item = (name: string) => within(menu).getByRole("menuitem", { name });
    // build is idle: Delete Ends it.
    await waitFor(() => expect(item("End")).toHaveAttribute("aria-keyshortcuts", "Delete"));
    // Drawn after the verb, out of its name.
    expect(item("End")).toHaveTextContent("EndDel");
    expect(item("Hide")).not.toHaveAttribute("aria-keyshortcuts");
    await user.keyboard("{Escape}");

    // review is running: Delete Hides it.
    act(() => pill("review").focus());
    await user.keyboard("{Shift>}{F10}{/Shift}");
    const menu2 = await screen.findByRole("menu");
    await waitFor(() =>
      expect(within(menu2).getByRole("menuitem", { name: "Hide" })).toHaveAttribute(
        "aria-keyshortcuts",
        "Delete",
      ),
    );
  });

  it("Delete and Backspace inside the rename box edit the name, never close the tab", async () => {
    platform.mockReturnValue("MacIntel");
    const user = userEvent.setup();
    const onCloseTab = vi.fn();
    render(<Tile onCloseTab={onCloseTab} />);
    act(() => pill("build").focus());
    act(() => void renameFocusedTab());
    const box = await screen.findByRole("textbox", { name: "Tab name" });
    await user.keyboard("{End}{Backspace}{Home}{Delete}");
    expect(box).toHaveValue("uil");
    expect(onCloseTab).not.toHaveBeenCalled();
  });

  it("a cancelled End keeps focus where the user puts it — no late jump to a neighbour", async () => {
    const user = userEvent.setup();
    const onCloseTab = vi.fn(async () => false); // End's ask, cancelled
    render(<Tile onCloseTab={onCloseTab} />);
    const reviewSession = "s-review";
    act(() => pill("review").focus());
    await user.keyboard("{Delete}");
    await waitFor(() => expect(onCloseTab).toHaveBeenCalledTimes(1));
    await act(async () => {});
    act(() => pill("build").focus());
    // Later the tab closes some other way: focus stays on build.
    act(() => void terminalHost().closeTab(reviewSession, "kill"));
    await waitFor(() => expect(screen.getAllByRole("tab")).toHaveLength(2));
    expect(pill("build")).toHaveFocus();
  });
});
