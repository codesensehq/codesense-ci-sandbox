// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  ROW_POPUP_ALLOWED,
  SETTINGS_TAB_BLOCKED,
  SETTINGS_TAB_WORKSPACE_ALLOWED,
  isDispatchable,
  type DispatchGateState,
} from "./is-dispatchable";
import { SHORTCUT_ACTIONS, type ShortcutAction } from "./registry";

const action = (scope: ShortcutAction["scope"]): ShortcutAction => ({
  id: `test.${scope}`,
  label: scope,
  defaultKeystroke: "CmdOrCtrl+K",
  scope,
  run: () => {},
});

/** The real registry entry for `id` (so a test also pins its scope). */
const registered = (id: string): ShortcutAction => {
  const found = SHORTCUT_ACTIONS.find((a) => a.id === id);
  if (!found) throw new Error(`no registered action ${id}`);
  return found;
};

/** Team actions whose target stays on screen around the Settings tab. */
const TEAM_ALLOWED_ON_SETTINGS = [
  "nav.open-workspace",
  "nav.quick-open",
  "nav.open-note-switcher",
  "nav.focus-left-search",
  "viewer.toggle-terminal",
  "viewer.toggle-nav",
  "viewer.toggle-right-pane",
  "viewer.toggle-pane-full-screen",
  "viewer.toggle-console",
  "viewer.cycle-foreground-layout",
  "weft.jump-to-next-needs-you",
  "viewer.focus-tile-question",
  "viewer.toggle-inbox-rollup",
  ...Array.from({ length: 9 }, (_, i) => `viewer.jump-workspace-${i + 1}`),
  "viewer.flip-last-workspace",
  "viewer.reopen-closed-tab",
  "story.launch-agent",
];

/** Team actions that act on the active document, which Settings is not. */
const TEAM_BLOCKED_ON_SETTINGS = ["nav.goto-heading", "viewer.edit-file"];

/** Workspace actions whose target is the terminal section, on screen beneath Settings. */
const WORKSPACE_ALLOWED_ON_SETTINGS = [
  "viewer.toggle-story-hud",
  "viewer.toggle-story-hud-size",
  "viewer.toggle-source-diff",
  "viewer.toggle-code-editor",
  "viewer.toggle-agent-inbox",
  "viewer.open-group-chat",
  "viewer.toggle-terminal-zoom",
  "viewer.cycle-workspace-next",
  "viewer.cycle-workspace-prev",
  "viewer.cycle-terminal-tab-next",
  "viewer.cycle-terminal-tab-prev",
  "viewer.cycle-epic-next",
  "viewer.cycle-epic-prev",
  "viewer.focus-pane-left",
  "viewer.focus-pane-right",
  "viewer.focus-pane-up",
  "viewer.focus-pane-down",
  "viewer.jump-root-workspace",
  "viewer.close-workspace",
  "viewer.hard-close-workspace",
  "viewer.new-terminal-tab",
  "viewer.new-terminal-on-machine",
  "viewer.split-terminal",
  "viewer.rename-terminal-tab",
  "viewer.find-in-terminal",
  "viewer.clear-terminal",
  ...Array.from({ length: 9 }, (_, i) => `viewer.jump-terminal-tab-${i + 1}`),
];
/** Workspace actions on the active viewer document, which Settings is not. */
const WORKSPACE_BLOCKED_ON_SETTINGS = ["viewer.export-pdf"];

const state = (overrides: Partial<DispatchGateState> = {}): DispatchGateState => ({
  settingsPanelOpen: false,
  hasActiveWorkspace: true,
  hasTeam: true,
  rowPopupFocused: false,
  ...overrides,
});

const NO_WORKSPACE = { hasActiveWorkspace: false } as const;

/** The global actions that open on top of a row popup and change nothing behind it. */
const ALLOWED_IN_ROW_POPUP = [
  "settings.open",
  "nav.open-command-palette",
  "nav.open-machines",
  "nav.open-source-repos",
  "nav.open-teams",
  "nav.open-workspaces",
];

describe("isDispatchable", () => {
  describe("with focus in a row popup (the quick view or the inbox chat)", () => {
    const inPopup = () => state({ rowPopupFocused: true });

    it("blocks every registered action but a short list of global ones", () => {
      expect([...ROW_POPUP_ALLOWED].sort()).toEqual([...ALLOWED_IN_ROW_POPUP].sort());
      for (const a of SHORTCUT_ACTIONS) {
        expect(isDispatchable(a, inPopup()), a.id).toBe(ALLOWED_IN_ROW_POPUP.includes(a.id));
        if (ALLOWED_IN_ROW_POPUP.includes(a.id)) expect(a.scope, a.id).toBe("global");
      }
      // The same actions dispatch again once focus leaves the popup.
      for (const a of SHORTCUT_ACTIONS) expect(isDispatchable(a, state()), a.id).toBe(true);
    });

    it("blocks ⌘[ / ⌘] back and forward and ⌘⇧[ / ⌘⇧] tab cycling", () => {
      for (const id of [
        "nav.context-back",
        "nav.context-forward",
        "viewer.cycle-terminal-tab-next",
        "viewer.cycle-terminal-tab-prev",
      ]) {
        expect(isDispatchable(registered(id), inPopup()), id).toBe(false);
        expect(isDispatchable(registered(id), state()), id).toBe(true);
      }
    });

    it("blocks the switchers, which change the team the tray and workspaces belong to", () => {
      expect(isDispatchable(registered("nav.switch-org"), inPopup())).toBe(false);
      expect(isDispatchable(registered("nav.switch-team"), inPopup())).toBe(false);
      expect(isDispatchable(action("global"), inPopup())).toBe(false);
    });
  });

  describe("the workspace an action needs is the active team's", () => {
    it("every workspace action fires with one focused there, and none without", () => {
      for (const a of SHORTCUT_ACTIONS.filter((x) => x.scope === "workspace")) {
        expect(isDispatchable(a, state()), a.id).toBe(true);
        expect(isDispatchable(a, state({ hasActiveWorkspace: false })), a.id).toBe(false);
      }
    });
  });

  it("always dispatches global actions", () => {
    expect(isDispatchable(action("global"), state())).toBe(true);
    expect(isDispatchable(action("global"), state({ settingsPanelOpen: true }))).toBe(true);
    expect(isDispatchable(action("global"), state({ ...NO_WORKSPACE, hasTeam: false }))).toBe(true);
  });

  describe("workspace scope", () => {
    it("requires a focused workspace", () => {
      expect(isDispatchable(action("workspace"), state({ ...NO_WORKSPACE }))).toBe(false);
    });

    it("is suppressed on the Settings tab unless allowlisted", () => {
      expect(isDispatchable(action("workspace"), state({ settingsPanelOpen: true }))).toBe(false);
    });

    it("classifies every registered workspace action for the Settings tab", () => {
      // A new workspace action fails here until someone files it under allowed or blocked.
      const ids = SHORTCUT_ACTIONS.filter((x) => x.scope === "workspace")
        .map((x) => x.id)
        .sort();
      expect(ids).toEqual(
        [...WORKSPACE_ALLOWED_ON_SETTINGS, ...WORKSPACE_BLOCKED_ON_SETTINGS].sort(),
      );
      expect([...SETTINGS_TAB_WORKSPACE_ALLOWED].sort()).toEqual(
        [...WORKSPACE_ALLOWED_ON_SETTINGS].sort(),
      );
      const onSettings = state({ settingsPanelOpen: true });
      for (const id of WORKSPACE_ALLOWED_ON_SETTINGS) {
        expect(isDispatchable(registered(id), onSettings), id).toBe(true);
        // Still needs a focused workspace.
        expect(
          isDispatchable(registered(id), state({ settingsPanelOpen: true, ...NO_WORKSPACE })),
          id,
        ).toBe(false);
      }
      for (const id of WORKSPACE_BLOCKED_ON_SETTINGS) {
        expect(isDispatchable(registered(id), onSettings), id).toBe(false);
        expect(isDispatchable(registered(id), state()), id).toBe(true);
      }
    });

    it("dispatches when a workspace is focused and Settings is closed", () => {
      expect(isDispatchable(action("workspace"), state())).toBe(true);
    });
  });

  describe("team scope", () => {
    it("requires an open team", () => {
      expect(isDispatchable(action("team"), state({ hasTeam: false }))).toBe(false);
    });

    it("dispatches on the Settings tab: the team view stays on screen around it", () => {
      expect(isDispatchable(action("team"), state({ settingsPanelOpen: true }))).toBe(true);
      const toggle = registered("viewer.toggle-terminal");
      expect(toggle.scope).toBe("team");
      expect(isDispatchable(toggle, state({ settingsPanelOpen: true }))).toBe(true);
    });

    it("still requires an open team on the Settings tab", () => {
      expect(
        isDispatchable(action("team"), state({ settingsPanelOpen: true, hasTeam: false })),
      ).toBe(false);
    });

    it.each(TEAM_BLOCKED_ON_SETTINGS)(
      "keeps the document-only action %s blocked on the Settings tab",
      (id) => {
        const a = registered(id);
        expect(a.scope).toBe("team");
        expect(isDispatchable(a, state({ settingsPanelOpen: true }))).toBe(false);
        expect(isDispatchable(a, state())).toBe(true);
      },
    );

    it("classifies every registered team action for the Settings tab", () => {
      // A new team action fails here until someone files it under allowed or blocked.
      const teamIds = SHORTCUT_ACTIONS.filter((x) => x.scope === "team")
        .map((x) => x.id)
        .sort();
      expect(teamIds).toEqual([...TEAM_ALLOWED_ON_SETTINGS, ...TEAM_BLOCKED_ON_SETTINGS].sort());
      expect([...SETTINGS_TAB_BLOCKED].sort()).toEqual([...TEAM_BLOCKED_ON_SETTINGS].sort());
      for (const id of TEAM_ALLOWED_ON_SETTINGS) {
        expect(isDispatchable(registered(id), state({ settingsPanelOpen: true })), id).toBe(true);
      }
      for (const id of TEAM_BLOCKED_ON_SETTINGS) {
        expect(isDispatchable(registered(id), state({ settingsPanelOpen: true })), id).toBe(false);
      }
    });

    it("dispatches from the empty roster (open team, no focused workspace)", () => {
      expect(isDispatchable(action("team"), state({ ...NO_WORKSPACE }))).toBe(true);
    });
  });
});
