// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { isReservedKeystroke, matchesKeystroke, parseKeystroke } from "./keystroke";
import {
  findConflict,
  resolveBindings,
  NAV_AXES,
  NAV_MENU_ITEMS,
  SHORTCUT_ACTIONS,
  type ShortcutAction,
} from "./registry";

describe("SHORTCUT_ACTIONS", () => {
  it("keeps Inbox in View → Panels with its persisted id, default chord and a replaceable binding", () => {
    const action = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-inbox-rollup")!;
    expect(action.menuPath).toEqual(["View", "Panels → Inbox"]);
    expect(action.defaultKeystroke).toBe("CmdOrCtrl+Shift+I");
    const binding = resolveBindings({ [action.id]: "CmdOrCtrl+Alt+I" }).get(action.id)!;
    expect(binding.keystroke).toBe("CmdOrCtrl+Alt+I");
  });

  it("contains exactly 73 configurable actions", () => {
    // SW-EW-33: 17 − 1 (viewer.toggle-workspace-panel) + 9 (viewer.jump-workspace-1..9)
    // + 1 (viewer.flip-last-workspace) + 1 (viewer.hard-close-workspace)
    // − 1 (nav.switch-workspace retired) + 1 (nav.open-workspace)
    // + 4 (viewer.focus-pane-left/right/up/down).
    // SW-EW-35: + 1 (viewer.toggle-console).
    // SW-GS-7: + 1 (viewer.toggle-sync), later retired with the sync view: − 1.
    // SW-SN-3: + 1 (nav.open-note-switcher).
    // SW-SN-6: + 1 (nav.goto-heading).
    // SLW-NC-3: − 1 (the install-workbench open action retired with the workbench).
    // SLW-WD-2: + 1 (viewer.toggle-source-diff).
    // SW-HB-6: + 1 (nav.open-command-palette).
    // SW-PP-3: + 1 (viewer.fit-pipeline).
    // SW-VE-8: + 1 (viewer.edit-file — ⌘E promoted from an inline keydown).
    // SW-CW-7: + 1 (viewer.cycle-foreground-layout).
    // OC-6: + 2 (viewer.cycle-epic-next / viewer.cycle-epic-prev).
    // OC-7: + 2 (viewer.cycle-terminal-tab-next / viewer.cycle-terminal-tab-prev).
    // SLW-DV-3: + 1 (viewer.toggle-code-editor — ⌘⇧C, standalone worktree editor).
    // SW-OC-1 (app-flow redesign): + 4 (nav.open-machines / nav.open-source-repos /
    // nav.open-teams / nav.open-workspaces — the management virtual-tab openers).
    // SW-WA-E12: + 4 (weft.dispatch / weft.toggle-posture / weft.arm-controller /
    // weft.toggle-gates — the Activity board's primary actions, promoted for hotkeys).
    // SLW-TL-4: − 1 (weft.dispatch retired — the dispatch-brief form is gone; new work
    // starts by opening an agent's CLI, not a form).
    // SW-LN-31: + 1 (story.launch-agent — launch the default agent on the story in focus).
    // SW-AV-5: + 1 (weft.jump-to-next-needs-you — cycle YOU-OWE rows → drill each agent).
    // SW-AV-6: − 3 (weft.toggle-posture / weft.arm-controller / weft.toggle-gates retired
    // with the Activity governance toolbar — the board SHOWS, it does not CONTROL).
    // + 1 (viewer.focus-tile-question — jump into a tile's agent-question banner).
    // SW-RM-UI-1: + 1 (nav.switch-machine — the machine picker, ⌘⌥⇧M).
    // + 15 terminal tab verbs: new / reopen / rename / find / clear / grid toggle
    // + viewer.jump-terminal-tab-1..9. ("What needs you" is the existing ⌘⇧J.)
    // SW-UX-CLI-10: + 1 (viewer.split-terminal — ⌘D, a new terminal side by side).
    // SLW-TL-4: - 1 (removed the dead weft.dispatch shortcut whose form no longer renders).
    // + 1 (viewer.new-terminal-on-machine — ⇧⌘N, the machine chooser; ⌘T is a quick tab).
    // − 1 (viewer.toggle-tab-grid — ⌘⇧M now steps in to all tabs and back out).
    // SW-OC-25: − 1 (viewer.fit-pipeline retired with the right-pane Pipeline panel).
    // SW-OC-28: − 1 (nav.switch-machine retired with the per-team machine picker).
    // SW-LNS-1: + 1 (nav.focus-left-search — ⌘⇧F focuses the pipeline-nav filter).
    // SW-TP CAP-8: + 1 (viewer.toggle-agent-inbox — per-tab inbox toggle, now ⌘I).
    // + 1 (viewer.toggle-inbox-rollup — ⌘⇧I, the right-pane Inbox).
    // SV-3: + 2 (nav.context-back / nav.context-forward — ⌘[ / ⌘] over teams and workspaces).
    // SV-PLG-13: + 1 (nav.quick-open — ⌘P file quick-open, so dashboards conflict with it).
    expect(SHORTCUT_ACTIONS).toHaveLength(73);
  });

  it("includes viewer.focus-tile-question — Jump to What Needs You (Cmd+Shift+J, team-scoped, conflict-free)", () => {
    const action = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.focus-tile-question");
    expect(action).toBeDefined();
    expect(action?.defaultKeystroke).toBe("CmdOrCtrl+Shift+J");
    expect(action?.scope).toBe("team");
    expect(action?.macCmdOnly).toBe(true);
    expect(action?.menuPath).toEqual(["Go", "Jump to What Needs You"]);
    expect(isReservedKeystroke("CmdOrCtrl+Shift+J")).toBeNull();
    expect(findConflict("CmdOrCtrl+Shift+J", {}, "viewer.focus-tile-question")).toBeNull();
  });

  it("SW-AV-5: includes weft.jump-to-next-needs-you (Cmd+Alt+Shift+J, team-scoped, conflict-free)", () => {
    const action = SHORTCUT_ACTIONS.find((a) => a.id === "weft.jump-to-next-needs-you");
    expect(action).toBeDefined();
    expect(action?.defaultKeystroke).toBe("CmdOrCtrl+Alt+Shift+J");
    expect(action?.scope).toBe("team");
    expect(action?.menuPath).toEqual(["Go", "Jump to Next Needs-You"]);
    expect(isReservedKeystroke("CmdOrCtrl+Alt+Shift+J")).toBeNull();
    expect(findConflict("CmdOrCtrl+Alt+Shift+J", {}, "weft.jump-to-next-needs-you")).toBeNull();
  });

  it("SLW-TL-4: the Activity-board Dispatch action is retired (the dispatch form is gone)", () => {
    expect(SHORTCUT_ACTIONS.find((a) => a.id === "weft.dispatch")).toBeUndefined();
  });

  it("SW-AV-6: the governance chords (posture / arm / gates) are retired with the toolbar", () => {
    for (const id of ["weft.toggle-posture", "weft.arm-controller", "weft.toggle-gates"]) {
      expect(
        SHORTCUT_ACTIONS.find((a) => a.id === id),
        id,
      ).toBeUndefined();
    }
  });

  it("SW-OC-28: has no machine switch (machines are picked per launch)", () => {
    expect(SHORTCUT_ACTIONS.some((a) => a.id === "nav.switch-machine")).toBe(false);
    expect(SHORTCUT_ACTIONS.some((a) => a.defaultKeystroke === "CmdOrCtrl+Alt+M")).toBe(false);
  });

  it("includes the OC-7 terminal-tab-cycle actions (Cmd+Shift+[ / ], workspace-scoped, conflict-free)", () => {
    const next = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.cycle-terminal-tab-next");
    const prev = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.cycle-terminal-tab-prev");
    // The bracket family's third axis: workspace Cmd+Alt+[/], epic Cmd+Alt+Shift+[/],
    // tab Cmd+Shift+[/]. (Ctrl+Tab is unrepresentable in this grammar — no Ctrl-only
    // token — and CmdOrCtrl+Tab is the macOS app switcher.)
    expect(next?.defaultKeystroke).toBe("CmdOrCtrl+Shift+]");
    expect(prev?.defaultKeystroke).toBe("CmdOrCtrl+Shift+[");
    expect(next?.scope).toBe("workspace");
    expect(prev?.scope).toBe("workspace");
    expect(next?.menuPath).toEqual(["Go", "Cycle Terminal Tab Next"]);
    expect(prev?.menuPath).toEqual(["Go", "Cycle Terminal Tab Prev"]);
    expect(isReservedKeystroke("CmdOrCtrl+Shift+]")).toBeNull();
    expect(isReservedKeystroke("CmdOrCtrl+Shift+[")).toBeNull();
    expect(findConflict("CmdOrCtrl+Shift+]", {}, "viewer.cycle-terminal-tab-next")).toBeNull();
    expect(findConflict("CmdOrCtrl+Shift+[", {}, "viewer.cycle-terminal-tab-prev")).toBeNull();
  });

  it("includes the OC-6 epic-nav actions (Cmd+Alt+Shift+[ / ], workspace-scoped, conflict-free)", () => {
    const next = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.cycle-epic-next");
    const prev = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.cycle-epic-prev");
    expect(next?.defaultKeystroke).toBe("CmdOrCtrl+Alt+Shift+]");
    expect(prev?.defaultKeystroke).toBe("CmdOrCtrl+Alt+Shift+[");
    // The Shift sibling of the workspace-cycle chord — moves BETWEEN epic rails.
    expect(next?.scope).toBe("workspace");
    expect(prev?.scope).toBe("workspace");
    expect(next?.menuPath).toEqual(["Go", "Cycle Epic Group Next"]);
    // No collision with the existing bindings (esp. workspace-cycle Cmd+Alt+[ / ]).
    expect(findConflict("CmdOrCtrl+Alt+Shift+]", {}, "viewer.cycle-epic-next")).toBeNull();
    expect(findConflict("CmdOrCtrl+Alt+Shift+[", {}, "viewer.cycle-epic-prev")).toBeNull();
  });

  it("includes the SW-CW-7 viewer.cycle-foreground-layout action (CmdOrCtrl+Shift+L, team-scoped, no conflict)", () => {
    const cycle = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.cycle-foreground-layout");
    expect(cycle).toBeDefined();
    // ⌘⇧L (L = Layout): a Shift+<letter> chord keeps its key so it actually fires,
    // and it's not terminal input so it needs no capture-phase intercept.
    expect(cycle?.defaultKeystroke).toBe("CmdOrCtrl+Shift+L");
    // team-scoped: cycles the active team's split arrangement, works whenever a
    // team is open (mirrors viewer.toggle-console).
    expect(cycle?.scope).toBe("team");
    expect(cycle?.menuPath).toEqual(["View", "Cycle Terminal Layout"]);
    expect(isReservedKeystroke("CmdOrCtrl+Shift+L")).toBeNull();
    expect(findConflict("CmdOrCtrl+Shift+L", {}, "viewer.cycle-foreground-layout")).toBeNull();
  });

  it("includes the SW-HB-6 nav.open-command-palette action (CmdOrCtrl+Shift+P, global-scoped, unreserved, no conflict)", () => {
    const palette = SHORTCUT_ACTIONS.find((a) => a.id === "nav.open-command-palette");
    expect(palette).toBeDefined();
    // ⌘⇧P — the VS-Code command-palette chord, freed when SW-EW-33 retired
    // nav.switch-workspace. NOT ⌘K: SW-FX-49 gave ⌘K to the workspace palette.
    expect(palette?.defaultKeystroke).toBe("CmdOrCtrl+Shift+P");
    // global-scoped: opens from the Home Base hub too (no active team required),
    // unlike the team-scoped workspace / note palettes.
    expect(palette?.scope).toBe("global");
    expect(palette?.menuPath).toEqual(["Go", "Command Palette"]);
    expect(isReservedKeystroke("CmdOrCtrl+Shift+P")).toBeNull();
    expect(findConflict("CmdOrCtrl+Shift+P", {}, "nav.open-command-palette")).toBeNull();
  });

  it("SW-LNS-1: includes nav.focus-left-search (CmdOrCtrl+Shift+F, team-scoped, unreserved, no conflict)", () => {
    const action = SHORTCUT_ACTIONS.find((a) => a.id === "nav.focus-left-search");
    expect(action).toBeDefined();
    expect(action?.defaultKeystroke).toBe("CmdOrCtrl+Shift+F");
    expect(action?.scope).toBe("team");
    expect(action?.menuPath).toEqual(["Go", "Focus Nav Search"]);
    expect(isReservedKeystroke("CmdOrCtrl+Shift+F")).toBeNull();
    expect(findConflict("CmdOrCtrl+Shift+F", {}, "nav.focus-left-search")).toBeNull();
  });

  it("includes the SW-EW-35 viewer.toggle-console action (CmdOrCtrl+Shift+\\, team-scoped, no conflict)", () => {
    const consoleToggle = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-console");
    expect(consoleToggle).toBeDefined();
    expect(consoleToggle?.defaultKeystroke).toBe("CmdOrCtrl+Shift+\\");
    // SW-EW-35: team-scoped so the console flip works in the empty state (last
    // workspace closed), not just when a workspace is focused.
    expect(consoleToggle?.scope).toBe("team");
    expect(consoleToggle?.menuPath).toEqual(["View", "Toggle Console"]);
    expect(findConflict("CmdOrCtrl+Shift+\\", {}, "viewer.toggle-console")).toBeNull();
  });

  it("includes the SW-EW-28 viewer.toggle-story-hud action (CmdOrCtrl+Shift+H, workspace-scoped, no conflict)", () => {
    const hud = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-story-hud");
    expect(hud).toBeDefined();
    expect(hud?.defaultKeystroke).toBe("CmdOrCtrl+Shift+H");
    expect(hud?.scope).toBe("workspace");
    expect(findConflict("CmdOrCtrl+Shift+H", {}, "viewer.toggle-story-hud")).toBeNull();
  });

  it("includes the SW-EW-28 viewer.toggle-story-hud-size action (CmdOrCtrl+Shift+U, workspace-scoped, no conflict)", () => {
    const size = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-story-hud-size");
    expect(size).toBeDefined();
    expect(size?.defaultKeystroke).toBe("CmdOrCtrl+Shift+U");
    expect(size?.scope).toBe("workspace");
    expect(findConflict("CmdOrCtrl+Shift+U", {}, "viewer.toggle-story-hud-size")).toBeNull();
  });

  it("includes the SLW-WD-2 viewer.toggle-source-diff action (CmdOrCtrl+Shift+D, workspace-scoped, no conflict)", () => {
    const sourceDiff = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-source-diff");
    expect(sourceDiff).toBeDefined();
    expect(sourceDiff?.defaultKeystroke).toBe("CmdOrCtrl+Shift+D");
    expect(sourceDiff?.scope).toBe("workspace");
    expect(sourceDiff?.menuPath).toEqual(["View", "Toggle Source Diff"]);
    expect(findConflict("CmdOrCtrl+Shift+D", {}, "viewer.toggle-source-diff")).toBeNull();
  });

  it("includes the SW-EW-4 viewer.toggle-terminal-zoom action (CmdOrCtrl+Shift+M, workspace-scoped, no conflict)", () => {
    const zoom = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-terminal-zoom");
    expect(zoom).toBeDefined();
    expect(zoom?.defaultKeystroke).toBe("CmdOrCtrl+Shift+M");
    expect(zoom?.scope).toBe("workspace");
    expect(findConflict("CmdOrCtrl+Shift+M", {}, "viewer.toggle-terminal-zoom")).toBeNull();
  });

  it("includes the SW-FX-8 viewer.toggle-nav action (default CmdOrCtrl+B, team-scoped, no conflict)", () => {
    const toggleNav = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-nav");
    expect(toggleNav).toBeDefined();
    expect(toggleNav?.defaultKeystroke).toBe("CmdOrCtrl+B");
    // SW-EW-35: team-scoped so the sidebar toggles in the empty state too.
    expect(toggleNav?.scope).toBe("team");
    // ⌘B must not collide with any existing binding.
    expect(findConflict("CmdOrCtrl+B", {}, "viewer.toggle-nav")).toBeNull();
  });

  it("no longer includes viewer.toggle-workspace-panel (removed with the left panel, SW-EW-33)", () => {
    expect(SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-workspace-panel")).toBeUndefined();
  });

  it("no longer includes nav.switch-workspace (retired SW-EW-33, dup of nav.open-workspace)", () => {
    expect(SHORTCUT_ACTIONS.find((a) => a.id === "nav.switch-workspace")).toBeUndefined();
  });

  it("includes the SW-EW-33 viewer.focus-pane-* directional actions (CmdOrCtrl+Alt+Arrow*, workspace-scoped, no conflict)", () => {
    const dirs = [
      ["left", "ArrowLeft"],
      ["right", "ArrowRight"],
      ["up", "ArrowUp"],
      ["down", "ArrowDown"],
    ] as const;
    for (const [dir, arrow] of dirs) {
      const id = `viewer.focus-pane-${dir}`;
      const action = SHORTCUT_ACTIONS.find((a) => a.id === id);
      expect(action, id).toBeDefined();
      expect(action?.defaultKeystroke).toBe(`CmdOrCtrl+Alt+${arrow}`);
      expect(action?.scope).toBe("workspace");
      expect(findConflict(`CmdOrCtrl+Alt+${arrow}`, {}, id)).toBeNull();
    }
  });

  it("includes the nav.open-workspace palette on CmdOrCtrl+K (SW-FX-49 re-key from ⌘⇧K, team-scoped, no conflict)", () => {
    const open = SHORTCUT_ACTIONS.find((a) => a.id === "nav.open-workspace");
    expect(open).toBeDefined();
    // SW-FX-49: ⌘K (was ⌘⇧K) — the deliberate workspace-open key, distinct from
    // ⌘P's file peek. ⌘K is legitimately contested across apps so it's unreserved.
    expect(open?.defaultKeystroke).toBe("CmdOrCtrl+K");
    // team-scoped: works in the empty/minimized state (open a workspace when none focused).
    expect(open?.scope).toBe("team");
    expect(isReservedKeystroke("CmdOrCtrl+K")).toBeNull();
    expect(findConflict("CmdOrCtrl+K", {}, "nav.open-workspace")).toBeNull();
  });

  it("nav.open-workspace matches ⌘K and no longer matches ⌘⇧K (SW-FX-49)", () => {
    const open = SHORTCUT_ACTIONS.find((a) => a.id === "nav.open-workspace");
    const keystroke = open?.defaultKeystroke ?? "";
    // Synthetic events model the real dispatcher's matchesKeystroke check.
    const cmdK = {
      metaKey: true,
      ctrlKey: false,
      shiftKey: false,
      altKey: false,
      key: "k",
      code: "KeyK",
    } as unknown as KeyboardEvent;
    const cmdShiftK = {
      metaKey: true,
      ctrlKey: false,
      shiftKey: true,
      altKey: false,
      key: "K",
      code: "KeyK",
    } as unknown as KeyboardEvent;
    expect(matchesKeystroke(cmdK, keystroke)).toBe(true);
    expect(matchesKeystroke(cmdShiftK, keystroke)).toBe(false);
  });

  it("includes the SW-SN-3 nav.open-note-switcher action (CmdOrCtrl+O, team-scoped, unreserved, no conflict)", () => {
    const noteSwitcher = SHORTCUT_ACTIONS.find((a) => a.id === "nav.open-note-switcher");
    expect(noteSwitcher).toBeDefined();
    expect(noteSwitcher?.defaultKeystroke).toBe("CmdOrCtrl+O");
    // team-scoped: the dialog mounts only in the active team's view (notes are
    // global, but the mount mirrors the workspace palette).
    expect(noteSwitcher?.scope).toBe("team");
    expect(noteSwitcher?.menuPath).toEqual(["Go", "Open Note Switcher"]);
    expect(isReservedKeystroke("CmdOrCtrl+O")).toBeNull();
    expect(findConflict("CmdOrCtrl+O", {}, "nav.open-note-switcher")).toBeNull();
  });

  it("includes the SW-SN-6 nav.goto-heading action (CmdOrCtrl+Shift+O, team-scoped, unreserved, no conflict)", () => {
    const gotoHeading = SHORTCUT_ACTIONS.find((a) => a.id === "nav.goto-heading");
    expect(gotoHeading).toBeDefined();
    // Took Cmd+Shift+O from nav.switch-org (rebound to Cmd+Shift+G) per the spec.
    expect(gotoHeading?.defaultKeystroke).toBe("CmdOrCtrl+Shift+O");
    expect(gotoHeading?.scope).toBe("team");
    // menuPath makes it discoverable + rebindable in Settings → Shortcuts.
    expect(gotoHeading?.menuPath).toEqual(["Go", "Go to Heading"]);
    expect(isReservedKeystroke("CmdOrCtrl+Shift+O")).toBeNull();
    expect(findConflict("CmdOrCtrl+Shift+O", {}, "nav.goto-heading")).toBeNull();
  });

  it("includes the terminal tab verbs at their default keys and scopes, conflict-free", () => {
    const expected: Record<string, [string, "workspace" | "team"]> = {
      "viewer.new-terminal-tab": ["CmdOrCtrl+T", "workspace"],
      "viewer.reopen-closed-tab": ["CmdOrCtrl+Shift+T", "team"],
      "viewer.rename-terminal-tab": ["F2", "workspace"],
      "viewer.find-in-terminal": ["CmdOrCtrl+F", "workspace"],
      "viewer.clear-terminal": ["CmdOrCtrl+Shift+K", "workspace"],
      "viewer.split-terminal": ["CmdOrCtrl+D", "workspace"],
      ...Object.fromEntries(
        Array.from({ length: 9 }, (_, i) => [
          `viewer.jump-terminal-tab-${i + 1}`,
          [`CmdOrCtrl+Alt+${i + 1}`, "workspace"],
        ]),
      ),
    };
    for (const [id, [keystroke, scope]] of Object.entries(expected)) {
      const action = SHORTCUT_ACTIONS.find((a) => a.id === id);
      expect(action?.defaultKeystroke, id).toBe(keystroke);
      expect(action?.scope, id).toBe(scope);
      expect(isReservedKeystroke(keystroke), id).toBeNull();
      expect(findConflict(keystroke, {}, id), id).toBeNull();
    }
  });

  it("moves nav.switch-team to CmdOrCtrl+Alt+Shift+T (⌘⇧T is Reopen Closed Tab)", () => {
    const switchTeam = SHORTCUT_ACTIONS.find((a) => a.id === "nav.switch-team");
    expect(switchTeam?.defaultKeystroke).toBe("CmdOrCtrl+Alt+Shift+T");
    expect(findConflict("CmdOrCtrl+Alt+Shift+T", {}, "nav.switch-team")).toBeNull();
  });

  it("has ONE what-needs-you chord: the ⌘⌥J variant is retired", () => {
    expect(SHORTCUT_ACTIONS.some((a) => a.id === "viewer.jump-next-needs-you-tab")).toBe(false);
    expect(findConflict("CmdOrCtrl+Alt+J", {}, "")).toBeNull();
  });

  it("marks every terminal tab verb ⌘-only on macOS", () => {
    const ids = NAV_AXES.find((a) => a.label === "Terminal Tab Actions")?.actionIds ?? [];
    const keyed = ids.filter((id) => id !== "viewer.rename-terminal-tab"); // F2: no modifier
    for (const id of keyed) {
      expect(SHORTCUT_ACTIONS.find((a) => a.id === id)?.macCmdOnly, id).toBe(true);
    }
  });

  it("keeps ⌘1..9 on the workspace jumps (tab N is ⌘⌥N)", () => {
    for (let n = 1; n <= 9; n++) {
      expect(findConflict(`CmdOrCtrl+${n}`, {}, "")).toBe(`viewer.jump-workspace-${n}`);
    }
  });

  it("rebinds nav.switch-org to CmdOrCtrl+Shift+G (SW-SN-6 freed Shift+O for go-to-heading)", () => {
    const switchOrg = SHORTCUT_ACTIONS.find((a) => a.id === "nav.switch-org");
    expect(switchOrg).toBeDefined();
    expect(switchOrg?.defaultKeystroke).toBe("CmdOrCtrl+Shift+G");
    expect(switchOrg?.scope).toBe("global");
    expect(isReservedKeystroke("CmdOrCtrl+Shift+G")).toBeNull();
    expect(findConflict("CmdOrCtrl+Shift+G", {}, "nav.switch-org")).toBeNull();
  });

  it("includes the SW-EW-33 positional jump actions viewer.jump-workspace-1..9 (CmdOrCtrl+N, team-scoped, no conflict)", () => {
    for (let n = 1; n <= 9; n++) {
      const id = `viewer.jump-workspace-${n}`;
      const action = SHORTCUT_ACTIONS.find((a) => a.id === id);
      expect(action, id).toBeDefined();
      expect(action?.defaultKeystroke).toBe(`CmdOrCtrl+${n}`);
      // `team` scope so it fires in the empty/minimized-roster state (reopen).
      expect(action?.scope).toBe("team");
      expect(findConflict(`CmdOrCtrl+${n}`, {}, id)).toBeNull();
    }
  });

  it("includes the SW-EW-33 viewer.flip-last-workspace action (CmdOrCtrl+Alt+;, team-scoped, no conflict)", () => {
    const flip = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.flip-last-workspace");
    expect(flip).toBeDefined();
    expect(flip?.defaultKeystroke).toBe("CmdOrCtrl+Alt+;");
    expect(flip?.scope).toBe("team");
    expect(findConflict("CmdOrCtrl+Alt+;", {}, "viewer.flip-last-workspace")).toBeNull();
  });

  it("includes the SW-EW-33 viewer.hard-close-workspace action (CmdOrCtrl+Shift+Backspace, workspace-scoped, no conflict)", () => {
    const hard = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.hard-close-workspace");
    expect(hard).toBeDefined();
    expect(hard?.defaultKeystroke).toBe("CmdOrCtrl+Shift+Backspace");
    // workspace-scoped: hard-close needs a focused workspace (unlike tray nav).
    expect(hard?.scope).toBe("workspace");
    expect(findConflict("CmdOrCtrl+Shift+Backspace", {}, "viewer.hard-close-workspace")).toBeNull();
  });

  it("every action has a unique id", () => {
    const ids = SHORTCUT_ACTIONS.map((a) => a.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("every bound default keystroke parses without error", () => {
    for (const action of SHORTCUT_ACTIONS.filter((a) => a.defaultKeystroke)) {
      expect(() => parseKeystroke(action.defaultKeystroke), action.id).not.toThrow();
    }
  });

  it("no default keystroke is a reserved combo", () => {
    for (const action of SHORTCUT_ACTIONS) {
      expect(isReservedKeystroke(action.defaultKeystroke), action.id).toBeNull();
    }
  });

  it("no two actions share a default keystroke", () => {
    const seen = new Map<string, string>();
    for (const action of SHORTCUT_ACTIONS) {
      const prev = seen.get(action.defaultKeystroke);
      expect(prev, `${action.id} conflicts with ${prev ?? ""}`).toBeUndefined();
      seen.set(action.defaultKeystroke, action.id);
    }
  });

  it("menuPath is a 2-tuple of [submenu, item] when present", () => {
    for (const action of SHORTCUT_ACTIONS) {
      if (!action.menuPath) continue;
      expect(action.menuPath).toHaveLength(2);
      expect(typeof action.menuPath[0]).toBe("string");
      expect(typeof action.menuPath[1]).toBe("string");
    }
  });

  it("every action has menuPath set", () => {
    for (const action of SHORTCUT_ACTIONS) {
      expect(action.menuPath, action.id).toBeDefined();
    }
  });

  it("menu paths use only the three expected submenus (File, View, Go)", () => {
    const allowed = new Set(["File", "View", "Go"]);
    for (const action of SHORTCUT_ACTIONS) {
      if (!action.menuPath) continue;
      expect(allowed.has(action.menuPath[0]), action.id).toBe(true);
    }
  });

  it("run() is a function for every action", () => {
    for (const action of SHORTCUT_ACTIONS) {
      expect(typeof action.run, action.id).toBe("function");
    }
  });

  it("scope is 'global', 'workspace', or 'team' for every action", () => {
    for (const action of SHORTCUT_ACTIONS) {
      expect(["global", "workspace", "team"]).toContain(action.scope);
    }
  });
});

describe("findConflict", () => {
  function findAction(id: string): ShortcutAction {
    const a = SHORTCUT_ACTIONS.find((x) => x.id === id);
    if (!a) throw new Error(`no action ${id}`);
    return a;
  }

  it("returns null when keystroke is unused", () => {
    expect(findConflict("CmdOrCtrl+Shift+Z", {}, "settings.open")).toBeNull();
  });

  it("returns the conflicting action's id when keystroke is already assigned", () => {
    const switchOrg = findAction("nav.switch-org");
    expect(findConflict(switchOrg.defaultKeystroke, {}, "settings.open")).toBe("nav.switch-org");
  });

  it("does NOT flag the same action against its own current keystroke", () => {
    const settingsOpen = findAction("settings.open");
    expect(findConflict(settingsOpen.defaultKeystroke, {}, "settings.open")).toBeNull();
  });

  it("honors user overrides in the shortcuts map (not just defaults)", () => {
    // `nav.switch-org` is rebound to a new keystroke; the OLD default should
    // now be free, and the NEW keystroke should be taken. (Uses switch-org's
    // current default CmdOrCtrl+Shift+G as the "old" one — SW-SN-6 gave Shift+O
    // to nav.goto-heading, so Shift+O is no longer a free chord to probe here.)
    const overrides = { "nav.switch-org": "CmdOrCtrl+Shift+R" };
    // Old default (CmdOrCtrl+Shift+G) is now free for another action to take
    expect(findConflict("CmdOrCtrl+Shift+G", overrides, "settings.open")).toBeNull();
    // New keystroke is now taken
    expect(findConflict("CmdOrCtrl+Shift+R", overrides, "settings.open")).toBe("nav.switch-org");
  });
});

describe("NAV_AXES", () => {
  const navIds = NAV_AXES.flatMap((axis) => axis.actionIds);

  it("references only ids that exist in SHORTCUT_ACTIONS", () => {
    const known = new Set(SHORTCUT_ACTIONS.map((a) => a.id));
    const missing = navIds.filter((id) => !known.has(id));
    expect(missing).toEqual([]);
  });

  it("lists each nav id exactly once (no duplicate across axes)", () => {
    expect(new Set(navIds).size).toBe(navIds.length);
  });

  it("covers all five movement axes plus positional jump and recency", () => {
    expect(NAV_AXES.map((a) => a.label)).toEqual([
      // SV-3: back and forward over the teams and workspaces switched between.
      "Team History",
      "Story Workspace",
      "Terminal Tab",
      "Epic Group",
      "Pane Focus",
      "Workspace Position",
      "Recent Workspace",
      "Terminal Tab Actions",
      // SW-OC-1 (app-flow redesign): the management virtual-tab openers in the native Go menu.
      "Management",
    ]);
  });

  it("groups the positional jumps as root + Cmd+1..9 in order", () => {
    const positional = NAV_AXES.find((a) => a.label === "Workspace Position");
    expect(positional?.actionIds).toEqual([
      "viewer.jump-root-workspace",
      ...Array.from({ length: 9 }, (_, i) => `viewer.jump-workspace-${i + 1}`),
    ]);
  });

  it("omits non-navigation actions (close / edit / export are not axes)", () => {
    const navSet = new Set(navIds);
    for (const id of [
      "viewer.close-workspace",
      "viewer.hard-close-workspace",
      "viewer.edit-file",
      "viewer.export-pdf",
      "settings.open",
    ]) {
      expect(navSet.has(id)).toBe(false);
    }
  });

  it("has unique labels so axis slugs / React keys can't collide", () => {
    const labels = NAV_AXES.map((a) => a.label);
    expect(new Set(labels).size).toBe(labels.length);
    const slugs = labels.map((l) => l.toLowerCase().replace(/\s+/g, "-"));
    expect(new Set(slugs).size).toBe(slugs.length);
  });

  it("groups every nav-family action in the registry (reverse drift guard)", () => {
    // Any workspace/tab/epic/pane/jump/flip action MUST appear in an axis, or it
    // would silently fall into the ungrouped flat list. These are the OC-6/OC-7
    // nav families; a new action under one of them that is not added to NAV_AXES
    // fails here.
    const NAV_PREFIXES = [
      "viewer.cycle-workspace-",
      "viewer.cycle-terminal-tab-",
      "viewer.cycle-epic-",
      "viewer.focus-pane-",
      "viewer.jump-root-workspace",
      "viewer.jump-workspace-",
      "viewer.flip-last-workspace",
    ];
    const navSet = new Set(navIds);
    const ungrouped = SHORTCUT_ACTIONS.map((a) => a.id)
      .filter((id) => NAV_PREFIXES.some((p) => id.startsWith(p)))
      .filter((id) => !navSet.has(id));
    expect(ungrouped).toEqual([]);
  });
});

describe("NAV_MENU_ITEMS", () => {
  it("is NAV_AXES minus the nine positional workspace jumps and the off-menu axes, in axis order", () => {
    const positional = new Set(
      Array.from({ length: 9 }, (_, i) => `viewer.jump-workspace-${i + 1}`),
    );
    const expected = NAV_AXES.filter((axis) => axis.inNativeMenu !== false)
      .flatMap((axis) => axis.actionIds)
      .filter((id) => !positional.has(id));
    expect(NAV_MENU_ITEMS.map((item) => item.id)).toEqual(expected);
  });

  it("resolves every id to a real action and its menu label", () => {
    for (const item of NAV_MENU_ITEMS) {
      const action = SHORTCUT_ACTIONS.find((a) => a.id === item.id);
      expect(action).toBeDefined();
      expect(item.label).toBe(action?.menuPath?.[1] ?? action?.label);
    }
  });

  it("keeps the terminal tab verbs out of the native menu (its accelerators would take ⌘F / ⌘T / F2 before the page)", () => {
    const ids = NAV_MENU_ITEMS.map((item) => item.id);
    const tabVerbs = NAV_AXES.find((a) => a.label === "Terminal Tab Actions")?.actionIds ?? [];
    expect(tabVerbs.length).toBeGreaterThan(0);
    for (const id of tabVerbs) expect(ids).not.toContain(id);
  });

  it("excludes the positional jumps that stay in the Settings reference", () => {
    expect(NAV_MENU_ITEMS.some((item) => item.id.startsWith("viewer.jump-workspace-"))).toBe(false);
  });

  it("has unique ids that never collide with the hardcoded Go switchers", () => {
    // The pushed nav group and the hardcoded nav.switch-org/team share the Go
    // submenu; a duplicate id would make menu:action dispatch ambiguous.
    const ids = NAV_MENU_ITEMS.map((item) => item.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const hardcoded of ["nav.switch-org", "nav.switch-team"]) {
      expect(ids).not.toContain(hardcoded);
    }
  });
});
