// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

// The CLI shows the active team, so the verbs on its tiles and tabs act on the active team's
// focused workspace, and a team switch moves them with it. Another open team is never touched.
const h = vi.hoisted(() => ({
  newTerminalTab: vi.fn(),
  splitTerminal: vi.fn(() => true),
  requestNewWorkspaceTerminal: vi.fn(),
  hideWorkspace: vi.fn(),
  reopenClosedTab: vi.fn(() => true),
  jumpToWhatNeedsYou: vi.fn(() => true),
  revealOrToggleTerminal: vi.fn(),
}));
vi.mock("@/features/viewer/lib/terminal-tab-keys", async (importActual) => ({
  ...(await importActual<typeof import("@/features/viewer/lib/terminal-tab-keys")>()),
  newTerminalTab: h.newTerminalTab,
  splitTerminal: h.splitTerminal,
  reopenClosedTab: h.reopenClosedTab,
  jumpToWhatNeedsYou: h.jumpToWhatNeedsYou,
}));
vi.mock("@/features/viewer/lib/focus-cascade", async (importActual) => ({
  ...(await importActual<typeof import("@/features/viewer/lib/focus-cascade")>()),
  revealOrToggleTerminal: h.revealOrToggleTerminal,
}));
vi.mock("@/features/weft/stores/weft-jump-store", () => ({
  useWeftJumpStore: { getState: () => ({ jumpToNextNeedsYou: null }) },
}));
vi.mock("@/features/viewer/lib/new-workspace-terminal", () => ({
  requestNewWorkspaceTerminal: h.requestNewWorkspaceTerminal,
}));
vi.mock("@/features/viewer/lib/terminal-close", async (importActual) => ({
  ...(await importActual<typeof import("@/features/viewer/lib/terminal-close")>()),
  hideWorkspace: h.hideWorkspace,
}));

import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { SHORTCUT_ACTIONS } from "./registry";

const ACTIVE = "acme:default";
const OTHER = "acme:other";

const run = (id: string) => {
  const action = SHORTCUT_ACTIONS.find((a) => a.id === id);
  if (!action) throw new Error(`${id} not registered`);
  action.run();
};

beforeEach(() => {
  vi.clearAllMocks();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  useViewerStore.setState({
    openedTeams: [ACTIVE, OTHER],
    activeWorkspace: { [ACTIVE]: "ws-a", [OTHER]: "ws-b" },
    workspaces: {
      [ACTIVE]: [{ id: "ws-a", kind: "story", cwd: "/a", status: "ready" }],
      [OTHER]: [{ id: "ws-b", kind: "story", cwd: "/b", status: "ready" }],
    },
  });
});

describe("terminal verbs act on the active team's focused workspace", () => {
  it("⌘T, New Terminal On…, ⌘D and ⇧⌘W act on the active team's workspace", () => {
    run("viewer.new-terminal-tab");
    expect(h.newTerminalTab).toHaveBeenCalledWith(ACTIVE, "ws-a");
    run("viewer.new-terminal-on-machine");
    expect(h.requestNewWorkspaceTerminal).toHaveBeenCalledWith(ACTIVE, "ws-a");
    run("viewer.split-terminal");
    expect(h.splitTerminal).toHaveBeenCalledWith(ACTIVE);
    run("viewer.close-workspace");
    expect(h.hideWorkspace).toHaveBeenCalledWith(ACTIVE, "ws-a");
  });

  it("move with a team switch to the new team's focused workspace", () => {
    useOrgStore.setState({ activeTeamName: "other" });
    run("viewer.new-terminal-tab");
    expect(h.newTerminalTab).toHaveBeenCalledWith(OTHER, "ws-b");
    run("viewer.close-workspace");
    expect(h.hideWorkspace).toHaveBeenCalledWith(OTHER, "ws-b");
  });
});

describe("the CLI's layout, tabs and jumps act on the active team", () => {
  it("⇧⌘T reopens a tab closed in the active team", () => {
    run("viewer.reopen-closed-tab");
    expect(h.reopenClosedTab).toHaveBeenCalledWith(ACTIVE);
  });

  it("⇧⌘L cycles the active team's layout", () => {
    const cycleForegroundPreset = vi.fn();
    useViewerStore.setState({ cycleForegroundPreset });
    run("viewer.cycle-foreground-layout");
    expect(cycleForegroundPreset).toHaveBeenCalledWith(ACTIVE);
  });

  it("⌘\\ reveals or toggles the active team's terminal and ⇧⌘\\ the one console layout", () => {
    run("viewer.toggle-terminal");
    expect(h.revealOrToggleTerminal).toHaveBeenCalledWith(ACTIVE);
    const toggleConsoleShared = vi.fn();
    useViewerStore.setState({ toggleConsoleShared });
    run("viewer.toggle-console");
    expect(toggleConsoleShared).toHaveBeenCalledWith();
  });

  it("⇧⌘J and ⌥⇧⌘J jump to what needs you in the active team", async () => {
    run("viewer.focus-tile-question");
    expect(h.jumpToWhatNeedsYou).toHaveBeenCalledWith(ACTIVE);
    h.jumpToWhatNeedsYou.mockClear();
    run("weft.jump-to-next-needs-you");
    await vi.waitFor(() => expect(h.jumpToWhatNeedsYou).toHaveBeenCalledWith(ACTIVE));
  });
});
