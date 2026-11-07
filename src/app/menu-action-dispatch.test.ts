// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ShortcutAction } from "@/shared/lib/shortcuts/registry";
import { ROW_POPUP_ATTR } from "@/shared/lib/focus-utils";
import { NO_TEAM_SENTINEL } from "@/shared/lib/team-key";

const h = vi.hoisted(() => ({
  settingsPanelOpen: false,
  workspace: { id: "ws-1" } as { id: string } | null,
  teamKey: "team-1",
  handlePanel: vi.fn((_id: string) => false),
  runWorkspace: vi.fn(),
  runTeam: vi.fn(),
  runToggleTerminal: vi.fn(),
  runEditFile: vi.fn(),
  runNewTerminalTab: vi.fn(),
  runGlobal: vi.fn(),
  runCycleWorkspace: vi.fn(),
  runSettings: vi.fn(),
  runBack: vi.fn(),
  runForward: vi.fn(),
}));

vi.mock("@/features/shell/right-pane/panel-menu", () => ({
  handlePanelMenuAction: (id: string) => h.handlePanel(id),
}));
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  getActiveWorkspaceContext: () => ({ teamKey: h.teamKey, workspace: h.workspace }),
}));
vi.mock("@/shared/stores/ui-store", () => ({
  useUiStore: { getState: () => ({ settingsPanelOpen: h.settingsPanelOpen }) },
}));
vi.mock("@/shared/lib/shortcuts/registry", async (importOriginal) => {
  // Real entries (real id + scope) with a spied run, so the cases also pin their scope.
  const real = await importOriginal<typeof import("@/shared/lib/shortcuts/registry")>();
  const fromRegistry = (id: string, run: () => void): ShortcutAction => {
    const found = real.SHORTCUT_ACTIONS.find((a) => a.id === id);
    if (!found) throw new Error(`no registered action ${id}`);
    return { ...found, run };
  };
  const make = (id: string, scope: ShortcutAction["scope"], run: () => void): ShortcutAction => ({
    id,
    label: id,
    defaultKeystroke: "CmdOrCtrl+K",
    scope,
    run,
  });
  const actions = [
    make("act.workspace", "workspace", h.runWorkspace),
    make("act.team", "team", h.runTeam),
    fromRegistry("viewer.toggle-terminal", h.runToggleTerminal),
    fromRegistry("viewer.edit-file", h.runEditFile),
    fromRegistry("viewer.new-terminal-tab", h.runNewTerminalTab),
    fromRegistry("viewer.cycle-workspace-next", h.runCycleWorkspace),
    fromRegistry("settings.open", h.runSettings),
    fromRegistry("nav.context-back", h.runBack),
    fromRegistry("nav.context-forward", h.runForward),
    // Its real run: the case below checks what it changes.
    real.SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-nav")!,
    make("act.global", "global", h.runGlobal),
  ];
  return {
    SHORTCUT_ACTIONS: actions,
    findShortcutAction: (id: string) => actions.find((a) => a.id === id),
  };
});

import { dispatchMenuAction } from "./menu-action-dispatch";

describe("dispatchMenuAction", () => {
  beforeEach(() => {
    h.settingsPanelOpen = false;
    h.workspace = { id: "ws-1" };
    h.teamKey = "team-1";
    h.handlePanel.mockReset();
    h.handlePanel.mockReturnValue(false);
    h.runWorkspace.mockClear();
    h.runTeam.mockClear();
    h.runToggleTerminal.mockClear();
    h.runEditFile.mockClear();
    h.runNewTerminalTab.mockClear();
    h.runGlobal.mockClear();
    h.runCycleWorkspace.mockClear();
    h.runSettings.mockClear();
  });

  describe("with focus in a row popup of the Agents pane", () => {
    afterEach(() => {
      document.body.innerHTML = "";
    });
    function focusPopup() {
      const popup = document.createElement("div");
      popup.setAttribute(ROW_POPUP_ATTR, "");
      popup.innerHTML = "<textarea></textarea>";
      document.body.appendChild(popup);
      popup.querySelector("textarea")!.focus();
    }

    it("a menu accelerator for a CLI, tile or workspace verb does nothing", () => {
      focusPopup();
      for (const id of ["viewer.cycle-workspace-next", "viewer.new-terminal-tab", "act.workspace"])
        dispatchMenuAction(id);
      dispatchMenuAction("act.team");
      dispatchMenuAction("viewer.toggle-terminal");
      dispatchMenuAction("act.global");
      expect(h.runCycleWorkspace).not.toHaveBeenCalled();
      expect(h.runNewTerminalTab).not.toHaveBeenCalled();
      expect(h.runWorkspace).not.toHaveBeenCalled();
      expect(h.runTeam).not.toHaveBeenCalled();
      expect(h.runToggleTerminal).not.toHaveBeenCalled();
      expect(h.runGlobal).not.toHaveBeenCalled();
    });

    it("Settings, which opens over it and changes nothing behind it, still opens", () => {
      focusPopup();
      dispatchMenuAction("settings.open");
      expect(h.runSettings).toHaveBeenCalledOnce();
    });
  });

  it("a workspace action needs the active team's focused workspace", () => {
    h.workspace = null;
    dispatchMenuAction("viewer.cycle-workspace-next");
    dispatchMenuAction("viewer.new-terminal-tab");
    expect(h.runCycleWorkspace).not.toHaveBeenCalled();
    expect(h.runNewTerminalTab).not.toHaveBeenCalled();

    h.workspace = { id: "ws-1" };
    dispatchMenuAction("viewer.cycle-workspace-next");
    dispatchMenuAction("viewer.new-terminal-tab");
    expect(h.runCycleWorkspace).toHaveBeenCalledOnce();
    expect(h.runNewTerminalTab).toHaveBeenCalledOnce();
  });

  it("dispatches a workspace-scoped action when a workspace is focused and Settings is closed", () => {
    dispatchMenuAction("act.workspace");
    expect(h.runWorkspace).toHaveBeenCalledOnce();
  });

  it("File → New Terminal Tab works on the Settings tab, still needing a focused workspace", () => {
    h.settingsPanelOpen = true;
    dispatchMenuAction("viewer.new-terminal-tab");
    expect(h.runNewTerminalTab).toHaveBeenCalledOnce();
    try {
      h.workspace = null;
      dispatchMenuAction("viewer.new-terminal-tab");
      expect(h.runNewTerminalTab).toHaveBeenCalledOnce();
    } finally {
      h.workspace = { id: "ws-1" };
    }
  });

  it("suppresses a workspace-scoped action outside the Settings allowlist while Settings is open", () => {
    h.settingsPanelOpen = true;
    dispatchMenuAction("act.workspace");
    expect(h.runWorkspace).not.toHaveBeenCalled();
  });

  it("suppresses a workspace-scoped action when no workspace is focused", () => {
    h.workspace = null;
    dispatchMenuAction("act.workspace");
    expect(h.runWorkspace).not.toHaveBeenCalled();
  });

  it("dispatches a team-scoped action on the Settings tab (the team view stays on screen)", () => {
    h.settingsPanelOpen = true;
    dispatchMenuAction("act.team");
    expect(h.runTeam).toHaveBeenCalledOnce();
  });

  it("View → Toggle Terminal works on the Settings tab", () => {
    h.settingsPanelOpen = true;
    dispatchMenuAction("viewer.toggle-terminal");
    expect(h.runToggleTerminal).toHaveBeenCalledOnce();
  });

  it("keeps File → Edit File blocked on the Settings tab (Settings is not a file)", () => {
    h.settingsPanelOpen = true;
    dispatchMenuAction("viewer.edit-file");
    expect(h.runEditFile).not.toHaveBeenCalled();
    // Off the Settings tab the same menu item dispatches.
    h.settingsPanelOpen = false;
    dispatchMenuAction("viewer.edit-file");
    expect(h.runEditFile).toHaveBeenCalledOnce();
  });

  it("dispatches a team-scoped action from the empty roster (no focused workspace)", () => {
    h.workspace = null;
    dispatchMenuAction("act.team");
    expect(h.runTeam).toHaveBeenCalledOnce();
  });

  it("suppresses a team-scoped action when no team is open", () => {
    h.teamKey = NO_TEAM_SENTINEL;
    dispatchMenuAction("act.team");
    expect(h.runTeam).not.toHaveBeenCalled();
  });

  it("routes panel toggles to the panel handler without touching the registry", () => {
    h.handlePanel.mockReturnValue(true);
    dispatchMenuAction("panel:agent");
    expect(h.handlePanel).toHaveBeenCalledWith("panel:agent");
    expect(h.runWorkspace).not.toHaveBeenCalled();
    expect(h.runTeam).not.toHaveBeenCalled();
    expect(h.runGlobal).not.toHaveBeenCalled();
  });

  it("ignores an unknown action id", () => {
    expect(() => dispatchMenuAction("does.not.exist")).not.toThrow();
  });
});

it.each(["cm-editor", "ProseMirror"])(
  "native Back/Forward yield to the focused %s editor",
  (className) => {
    h.runBack.mockClear();
    h.runForward.mockClear();
    document.body.innerHTML = `<div class="${className}"><textarea></textarea></div>`;
    document.querySelector("textarea")!.focus();
    try {
      dispatchMenuAction("nav.context-back");
      dispatchMenuAction("nav.context-forward");
      expect(h.runBack).not.toHaveBeenCalled();
      expect(h.runForward).not.toHaveBeenCalled();
    } finally {
      document.body.innerHTML = "";
    }
  },
);
