// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, fireEvent, act, screen } from "@testing-library/react";

import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { buildTeamKey } from "@/shared/lib/team-key";
import { ROW_POPUP_ATTR, TERMINAL_POPUP_ATTR } from "@/shared/lib/focus-utils";
import { matchShortcut, useShortcutHandler } from "./use-shortcut-handler";

it("the real split action yields to a forwarded page field", () => {
  const gate = {
    settingsPanelOpen: false,
    hasActiveWorkspace: true,
    hasTeam: true,
    rowPopupFocused: false,
  };
  const key = new KeyboardEvent("keydown", { key: "d", code: "KeyD", metaKey: true });
  expect(matchShortcut(key, {}, gate, false, true)).toBeUndefined();
  expect(matchShortcut(key, {}, gate, true, false)).toBeUndefined();
  expect(matchShortcut(key, {}, gate, false, false)?.id).toBe("viewer.split-terminal");
});

const TEST_TEAM_KEY = buildTeamKey("acme", "core");
function makeRoot(slug = "acme/web"): Workspace {
  return {
    id: `root:${slug}`,
    kind: "root",
    cwd: `/Users/dev/repos/${slug.split("/").pop()}`,
    status: "ready",
    sourceRepoSlug: slug,
  };
}
function seedActiveWorkspace() {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "core" });
  const root = makeRoot();
  useViewerStore.setState({
    workspaces: { [TEST_TEAM_KEY]: [root] },
    activeWorkspace: { [TEST_TEAM_KEY]: root.id },
  });
}
function clearActiveWorkspace() {
  useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
  useViewerStore.setState({ workspaces: {}, activeWorkspace: {} });
}

// ---------- Test rig ----------

function Harness() {
  useShortcutHandler();
  return null;
}

// Spy on one action's `run` per test, restore afterward.
function spyAction(actionId: string) {
  const action = SHORTCUT_ACTIONS.find((a) => a.id === actionId)!;
  const original = action.run;
  const spy = vi.fn();
  (action as { run: () => void }).run = spy;
  return {
    spy,
    restore: () => {
      (action as { run: () => void }).run = original;
    },
  };
}

// ---------- Setup ----------

beforeEach(() => {
  useShortcutsStore.setState({ shortcuts: {}, stateLoaded: true });
  clearActiveWorkspace();
  useUiStore.setState({ settingsPanelOpen: false });
});

// ---------- Tests ----------

describe("useShortcutHandler", () => {
  it("a workspace action needs a workspace in the active team: another team's does not count", () => {
    const other = buildTeamKey("acme", "other");
    const ws: Workspace = { id: "s-1", kind: "story", cwd: "/s-1", status: "ready" };
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "core" });
    useViewerStore.setState({
      openedTeams: [TEST_TEAM_KEY, other],
      workspaces: { [TEST_TEAM_KEY]: [], [other]: [ws] },
      activeWorkspace: { [other]: ws.id },
      softClosedWorkspaces: {},
    });
    const newTab = spyAction("viewer.new-terminal-tab");
    try {
      render(<Harness />);
      const t = new KeyboardEvent("keydown", {
        key: "t",
        code: "KeyT",
        metaKey: true,
        bubbles: true,
        cancelable: true,
      });
      act(() => {
        document.dispatchEvent(t);
      });
      // The key falls through: nothing in the active team could take it.
      expect(newTab.spy).not.toHaveBeenCalled();
      expect(t.defaultPrevented).toBe(false);
      // Once that team is the active one, ⌘T acts on its workspace.
      act(() => useOrgStore.setState({ activeTeamName: "other" }));
      const again = new KeyboardEvent("keydown", {
        key: "t",
        code: "KeyT",
        metaKey: true,
        bubbles: true,
        cancelable: true,
      });
      act(() => {
        document.dispatchEvent(again);
      });
      expect(newTab.spy).toHaveBeenCalledTimes(1);
      expect(again.defaultPrevented).toBe(true);
    } finally {
      newTab.restore();
      useViewerStore.setState({ openedTeams: [] });
    }
  });

  describe("with focus in a row popup of the Agents pane, no CLI, tile, tray or workspace key acts", () => {
    const keys: [string, Partial<KeyboardEventInit>][] = [
      ["viewer.new-terminal-tab", { key: "t", code: "KeyT" }],
      ["viewer.find-in-terminal", { key: "f", code: "KeyF" }],
      ["nav.open-workspace", { key: "k", code: "KeyK" }],
      ["viewer.toggle-terminal-zoom", { key: "M", code: "KeyM", shiftKey: true }],
      ["viewer.cycle-foreground-layout", { key: "L", code: "KeyL", shiftKey: true }],
      ["viewer.jump-workspace-1", { key: "1", code: "Digit1" }],
      ["viewer.cycle-workspace-next", { key: "]", code: "BracketRight", altKey: true }],
      ["viewer.cycle-workspace-prev", { key: "[", code: "BracketLeft", altKey: true }],
      ["viewer.flip-last-workspace", { key: ";", code: "Semicolon", altKey: true }],
      ["viewer.jump-root-workspace", { key: "0", code: "Digit0" }],
      ["viewer.focus-tile-question", { key: "J", code: "KeyJ", shiftKey: true }],
      ["viewer.split-terminal", { key: "d", code: "KeyD" }],
      ["viewer.new-terminal-on-machine", { key: "N", code: "KeyN", shiftKey: true }],
      ["viewer.clear-terminal", { key: "K", code: "KeyK", shiftKey: true }],
      ["viewer.close-workspace", { key: "W", code: "KeyW", shiftKey: true }],
      ["viewer.hard-close-workspace", { key: "Backspace", code: "Backspace", shiftKey: true }],
      ["viewer.cycle-terminal-tab-next", { key: "}", code: "BracketRight", shiftKey: true }],
      ["viewer.cycle-terminal-tab-prev", { key: "{", code: "BracketLeft", shiftKey: true }],
      ["viewer.jump-terminal-tab-1", { key: "¡", code: "Digit1", altKey: true }],
      ["viewer.focus-pane-left", { key: "ArrowLeft", code: "ArrowLeft", altKey: true }],
    ];

    /** A focused textarea: in the quick view's xterm, in the inbox chat, or a plain one. */
    function focusTextarea(where: "quick view" | "inbox chat" | "outside"): HTMLTextAreaElement {
      const host = document.createElement("div");
      if (where === "quick view") host.setAttribute(TERMINAL_POPUP_ATTR, "");
      if (where !== "outside") host.setAttribute(ROW_POPUP_ATTR, "");
      host.innerHTML =
        where === "inbox chat"
          ? "<textarea></textarea>"
          : '<div class="xterm"><textarea></textarea></div>';
      document.body.appendChild(host);
      const textarea = host.querySelector("textarea")!;
      textarea.focus();
      return textarea;
    }

    describe.each(["quick view", "inbox chat"] as const)("the %s", (where) => {
      it.each(keys)("%s", (id, init) => {
        seedActiveWorkspace();
        const { spy, restore } = spyAction(id);
        try {
          render(<Harness />);
          const inPopup = focusTextarea(where);
          act(() => {
            fireEvent.keyDown(inPopup, { ...init, metaKey: true });
          });
          expect(spy).not.toHaveBeenCalled();
          // The same key outside the popup acts.
          const outside = focusTextarea("outside");
          act(() => {
            fireEvent.keyDown(outside, { ...init, metaKey: true });
          });
          expect(spy).toHaveBeenCalledTimes(1);
        } finally {
          restore();
          document.body.innerHTML = "";
        }
      });

      it.each([
        ["settings.open", { key: ",", code: "Comma" }],
        ["nav.open-command-palette", { key: "P", code: "KeyP", shiftKey: true }],
      ] as [string, Partial<KeyboardEventInit>][])("%s still opens over it", (id, init) => {
        seedActiveWorkspace();
        const { spy, restore } = spyAction(id);
        try {
          render(<Harness />);
          const inPopup = focusTextarea(where);
          act(() => {
            fireEvent.keyDown(inPopup, { ...init, metaKey: true });
          });
          expect(spy).toHaveBeenCalledTimes(1);
        } finally {
          restore();
          document.body.innerHTML = "";
        }
      });
    });
  });

  it("fires a global action's run() when its default keystroke is pressed", () => {
    const { spy, restore } = spyAction("settings.open");
    try {
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(document, { key: ",", metaKey: true });
      });
      expect(spy).toHaveBeenCalledTimes(1);
    } finally {
      restore();
    }
  });

  it("does NOT fire a workspace-scoped action when no workspace is focused", () => {
    // toggle-terminal-zoom is workspace-scoped (it needs a foreground tile),
    // unlike the team-scoped layout toggles below.
    const { spy, restore } = spyAction("viewer.toggle-terminal-zoom");
    try {
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(document, { key: "M", metaKey: true, shiftKey: true });
      });
      expect(spy).not.toHaveBeenCalled();
    } finally {
      restore();
    }
  });

  it("DOES fire a workspace-scoped action when a workspace IS focused", () => {
    const { spy, restore } = spyAction("viewer.toggle-terminal-zoom");
    try {
      seedActiveWorkspace();
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(document, { key: "M", metaKey: true, shiftKey: true });
      });
      expect(spy).toHaveBeenCalledTimes(1);
    } finally {
      restore();
    }
  });

  it("SW-EW-35: a team-scoped layout toggle (Cmd+\\) fires in the empty state — team active, no focused workspace", () => {
    const { spy, restore } = spyAction("viewer.toggle-terminal");
    try {
      // Last workspace closed → team open but nothing focused. The terminal /
      // right-pane / nav / console toggles are TEAM surfaces, so they must still
      // fire here (would have been skipped when they were workspace-scoped).
      useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "core" });
      useViewerStore.setState({ workspaces: {}, activeWorkspace: {} });
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(document, { key: "\\", metaKey: true });
      });
      expect(spy).toHaveBeenCalledTimes(1);
    } finally {
      restore();
    }
  });

  it("SW-EW-33: a team-scoped action (Cmd+1) fires with a team active but NO focused workspace", () => {
    const { spy, restore } = spyAction("viewer.jump-workspace-1");
    try {
      // Team open, but every workspace minimized → no focused workspace.
      useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "core" });
      useViewerStore.setState({ workspaces: {}, activeWorkspace: {} });
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(document, { key: "1", metaKey: true });
      });
      expect(spy).toHaveBeenCalledTimes(1); // would have been skipped if scope were "workspace"
    } finally {
      restore();
    }
  });

  it("SW-EW-33: a team-scoped action does NOT fire when no team is active", () => {
    const { spy, restore } = spyAction("viewer.jump-workspace-1");
    try {
      clearActiveWorkspace(); // also clears org/team
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(document, { key: "1", metaKey: true });
      });
      expect(spy).not.toHaveBeenCalled();
    } finally {
      restore();
    }
  });

  it("respects user override — new keystroke fires, old default does NOT", () => {
    const { spy, restore } = spyAction("settings.open");
    try {
      // Rebind settings.open to CmdOrCtrl+Shift+.
      useShortcutsStore.setState({
        shortcuts: { "settings.open": "CmdOrCtrl+Shift+." },
        stateLoaded: true,
      });
      render(<Harness />);
      // New keystroke → fires
      act(() => {
        fireEvent.keyDown(document, { key: ".", metaKey: true, shiftKey: true });
      });
      expect(spy).toHaveBeenCalledTimes(1);

      // Old default (CmdOrCtrl+,) → does NOT fire
      act(() => {
        fireEvent.keyDown(document, { key: ",", metaKey: true });
      });
      expect(spy).toHaveBeenCalledTimes(1);
    } finally {
      restore();
    }
  });

  it("preventDefault runs when a match is found", () => {
    render(<Harness />);
    let event: KeyboardEvent | null = null;
    const handler = (e: KeyboardEvent) => {
      event = e;
    };
    document.addEventListener("keydown", handler);
    act(() => {
      fireEvent.keyDown(document, { key: ",", metaKey: true });
    });
    document.removeEventListener("keydown", handler);
    // The global handler fires first (earlier registration on document)
    // and calls preventDefault before the observer listener sees it.
    expect(event).not.toBeNull();
    expect(event!.defaultPrevented).toBe(true);
  });

  it("leaves a key a focused text editor already handled to the editor (⌘I italic)", () => {
    const { spy, restore } = spyAction("settings.open");
    const editor = document.createElement("div");
    editor.className = "ProseMirror";
    editor.setAttribute("contenteditable", "true");
    document.body.appendChild(editor);
    const claim = (e: Event) => e.preventDefault();
    editor.addEventListener("keydown", claim);
    try {
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(editor, { key: ",", metaKey: true });
      });
      expect(spy).not.toHaveBeenCalled();
      // Not claimed by the editor: the shortcut still fires from inside it.
      editor.removeEventListener("keydown", claim);
      act(() => {
        fireEvent.keyDown(editor, { key: ",", metaKey: true });
      });
      expect(spy).toHaveBeenCalledTimes(1);
      // Claimed outside any editor (a plain button): the shortcut still fires.
      const button = document.createElement("button");
      document.body.appendChild(button);
      button.addEventListener("keydown", claim);
      act(() => {
        fireEvent.keyDown(button, { key: ",", metaKey: true });
      });
      expect(spy).toHaveBeenCalledTimes(2);
      button.remove();
    } finally {
      editor.remove();
      restore();
    }
  });

  it("treats any editable host (contenteditable, plaintext-only) as an editor, not ='false'", () => {
    const { spy, restore } = spyAction("settings.open");
    const claim = (e: Event) => e.preventDefault();
    const make = (value: string) => {
      const el = document.createElement("div");
      el.setAttribute("contenteditable", value);
      el.addEventListener("keydown", claim);
      document.body.appendChild(el);
      return el;
    };
    const bare = make("");
    const plain = make("plaintext-only");
    const off = make("false");
    try {
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(bare, { key: ",", metaKey: true });
        fireEvent.keyDown(plain, { key: ",", metaKey: true });
      });
      expect(spy).not.toHaveBeenCalled();
      act(() => {
        fireEvent.keyDown(off, { key: ",", metaKey: true });
      });
      expect(spy).toHaveBeenCalledTimes(1);
    } finally {
      for (const el of [bare, plain, off]) el.remove();
      restore();
    }
  });

  it("ignores keydown fired during IME composition", () => {
    const { spy, restore } = spyAction("settings.open");
    try {
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(document, { key: ",", metaKey: true, isComposing: true });
      });
      expect(spy).not.toHaveBeenCalled();
    } finally {
      restore();
    }
  });

  it("lets in-tree handlers claim a keystroke via stopPropagation (bubble phase)", () => {
    const { spy, restore } = spyAction("settings.open");
    try {
      const claimer = (e: KeyboardEvent) => {
        if (e.metaKey && e.key === ",") e.stopPropagation();
      };
      // Listener attached to body bubbles to document; if document's
      // global handler is also bubble-phase, stopPropagation keeps the
      // event from reaching it.
      document.body.addEventListener("keydown", claimer);
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(document.body, { key: ",", metaKey: true });
      });
      document.body.removeEventListener("keydown", claimer);
      expect(spy).not.toHaveBeenCalled();
    } finally {
      restore();
    }
  });

  it("does not match unrelated keys — no spurious dispatch", () => {
    const { spy, restore } = spyAction("settings.open");
    try {
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(document, { key: "a", metaKey: true });
      });
      expect(spy).not.toHaveBeenCalled();
    } finally {
      restore();
    }
  });

  it("does NOT fire a document workspace action (⌘⇧E export) on the Settings tab", () => {
    const { spy, restore } = spyAction("viewer.export-pdf");
    try {
      seedActiveWorkspace();
      useUiStore.setState({ settingsPanelOpen: true });
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(document, { key: "E", metaKey: true, shiftKey: true });
      });
      expect(spy).not.toHaveBeenCalled();
    } finally {
      restore();
    }
  });

  it("⌘T on the Settings tab opens a terminal tab in the focused workspace", () => {
    const { spy, restore } = spyAction("viewer.new-terminal-tab");
    try {
      seedActiveWorkspace();
      useUiStore.setState({ settingsPanelOpen: true });
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(document, { key: "t", metaKey: true });
      });
      expect(spy).toHaveBeenCalledTimes(1);
    } finally {
      restore();
    }
  });

  it("⌘\\ on the Settings tab reveals/focuses the existing CLI from non-CLI focus and hides from CLI focus", async () => {
    // SV-PBF-184: real listener + registry + action integration through ordinary
    // rendered DOM. Settings focus is outside the CLI host, so the chord reveals a
    // collapsed CLI and focuses its terminal; from actual CLI-host focus it toggles
    // to hide. No action spy or helper stub — the real store transitions prove it.
    seedActiveWorkspace();
    useViewerStore.setState({
      layoutMode: "viewer",
      terminalVisible: false,
      lastTerminalMode: "shared",
    });
    useUiStore.setState({ settingsPanelOpen: true });

    const { unmount } = render(
      <div>
        <Harness />
        <input data-testid="settings-control" />
        <div data-xterm-host>
          <div data-pane-id="root:acme/web">
            <div className="xterm">
              <textarea data-testid="terminal" defaultValue="terminal content" />
            </div>
          </div>
        </div>
      </div>,
    );

    const settingsControl = screen.getByTestId("settings-control");
    const terminal = screen.getByTestId("terminal");

    // 1. Settings/non-CLI focus with collapsed CLI -> reveal existing CLI and focus terminal.
    act(() => settingsControl.focus());
    expect(document.activeElement).toBe(settingsControl);

    act(() => {
      fireEvent.keyDown(document, { key: "\\", metaKey: true });
    });
    expect(useViewerStore.getState().layoutMode).toBe("shared");
    expect(useViewerStore.getState().terminalVisible).toBe(true);
    expect(useViewerStore.getState().activeWorkspace[TEST_TEAM_KEY]).toBe("root:acme/web");
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(r));
    });
    expect(document.activeElement).toBe(terminal);

    // 2. Settings focus again while CLI is visible -> keep visible and focus the SAME terminal.
    act(() => settingsControl.focus());
    expect(document.activeElement).toBe(settingsControl);

    act(() => {
      fireEvent.keyDown(document, { key: "\\", metaKey: true });
    });
    expect(useViewerStore.getState().layoutMode).toBe("shared");
    expect(useViewerStore.getState().terminalVisible).toBe(true);
    expect(useViewerStore.getState().activeWorkspace[TEST_TEAM_KEY]).toBe("root:acme/web");
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(r));
    });
    expect(document.activeElement).toBe(terminal);
    expect((terminal as HTMLTextAreaElement).value).toBe("terminal content");

    // 3. Actual CLI-host focus -> hide via the real listener/registry/action.
    act(() => terminal.focus());
    expect(document.activeElement).toBe(terminal);

    act(() => {
      fireEvent.keyDown(document, { key: "\\", metaKey: true });
    });
    expect(useViewerStore.getState().layoutMode).toBe("viewer");
    expect(useViewerStore.getState().terminalVisible).toBe(false);
    expect(useViewerStore.getState().activeWorkspace[TEST_TEAM_KEY]).toBe("root:acme/web");

    unmount();
  });

  it("keeps the document-only team action ⌘E blocked on the Settings tab", () => {
    const { spy, restore } = spyAction("viewer.edit-file");
    try {
      seedActiveWorkspace();
      useUiStore.setState({ settingsPanelOpen: true });
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(document, { key: "e", metaKey: true });
      });
      expect(spy).not.toHaveBeenCalled();
      // Off the Settings tab the same key dispatches.
      act(() => useUiStore.setState({ settingsPanelOpen: false }));
      act(() => {
        fireEvent.keyDown(document, { key: "e", metaKey: true });
      });
      expect(spy).toHaveBeenCalledTimes(1);
    } finally {
      restore();
    }
  });

  it("STILL fires a global action when Settings is open", () => {
    const { spy, restore } = spyAction("settings.open");
    try {
      useUiStore.setState({ settingsPanelOpen: true });
      render(<Harness />);
      act(() => {
        fireEvent.keyDown(document, { key: ",", metaKey: true });
      });
      expect(spy).toHaveBeenCalledTimes(1);
    } finally {
      restore();
    }
  });

  it("detaches the listener on unmount", () => {
    const { spy, restore } = spyAction("settings.open");
    try {
      const { unmount } = render(<Harness />);
      unmount();
      act(() => {
        fireEvent.keyDown(document, { key: ",", metaKey: true });
      });
      expect(spy).not.toHaveBeenCalled();
    } finally {
      restore();
    }
  });
});
