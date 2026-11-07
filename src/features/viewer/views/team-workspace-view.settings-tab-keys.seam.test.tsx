// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { installMockBackend } from "@/app/mock/install";
import { MOCK_ORG_SLUG } from "@/app/mock/fixtures";
import { SETTINGS_VIRTUAL_PATH } from "@/features/settings/lib/open-settings-tab";

import { ROW_POPUP_ATTR } from "@/shared/lib/focus-utils";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { attachRightPaneHost } from "@/test/right-pane-host";

import { useShortcutHandler } from "@/shared/hooks/use-shortcut-handler";
import { useWorkspaceCycleIntercept } from "../hooks/use-workspace-cycle-intercept";

import { TeamWorkspaceView } from "./team-workspace-view";

function ShortcutRoot() {
  useShortcutHandler();
  useWorkspaceCycleIntercept("/repo");
  return null;
}

type Internals = {
  invoke: (cmd: string, args?: Record<string, unknown>, options?: unknown) => Promise<unknown>;
};

const teamKey = `${MOCK_ORG_SLUG}:default`;
const NEIGHBOUR = "virtual://neighbour";

/**
 * Settings is a viewer tab, not an overlay: the team view's own keys (⌘⇧] / ⌘⇧[
 * tab cycling, ⌘` pane cycling) keep working while it is the active tab.
 */
describe("TeamWorkspaceView keys on the Settings tab", () => {
  const invoked: string[] = [];

  // The team view draws its right pane (which reads the notes) into the workbench's host.
  let releaseHost: () => void = () => {};
  beforeEach(() => {
    releaseHost = attachRightPaneHost(teamKey);
    invoked.length = 0;
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    installMockBackend();
    const internals = (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__;
    const invoke = internals.invoke.bind(internals);
    internals.invoke = (cmd, args, options) => {
      invoked.push(cmd);
      return invoke(cmd, args, options);
    };
    useOrgStore.setState({ activeOrgSlug: MOCK_ORG_SLUG, activeTeamName: "default" });
  });

  afterEach(async () => {
    cleanup();
    // Window listener cleanup resolves asynchronously; keep the mock backend alive
    // until its pending unlisten calls have completed.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    releaseHost();
    useUiStore.setState({ settingsPanelOpen: false });
    useViewerStore.setState({
      layoutMode: null,
      terminalVisible: false,
      lastTerminalMode: "shared",
      openTabs: {},
      activeTab: {},
    });
    useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  async function mountOnSettingsTab(rootOrder?: "first" | "last") {
    useUiStore.setState({ settingsPanelOpen: true });
    const view = (
      <TooltipProvider>
        <TeamWorkspaceView teamKey={teamKey} />
      </TooltipProvider>
    );
    const mounted = render(
      <>
        {rootOrder === "first" && <ShortcutRoot />}
        {view}
      </>,
    );
    await waitFor(() =>
      expect(invoked).toEqual(expect.arrayContaining(["agents_list", "list_notes"])),
    );
    act(() => {
      useViewerStore.setState((s) => ({
        openTabs: {
          ...s.openTabs,
          [teamKey]: [
            { name: "Settings", path: SETTINGS_VIRTUAL_PATH },
            { name: "Neighbour", path: NEIGHBOUR },
          ],
        },
        activeTab: { ...s.activeTab, [teamKey]: SETTINGS_VIRTUAL_PATH },
      }));
      useUiStore.setState({ settingsPanelOpen: true });
    });
    if (rootOrder === "last")
      mounted.rerender(
        <>
          {view}
          <ShortcutRoot />
        </>,
      );
  }

  it.each(["first", "last"] as const)(
    "shifted brackets cycle only viewer tabs with the root listener mounted %s",
    async (rootOrder) => {
      useViewerStore.setState({
        workspaces: {
          [teamKey]: [{ id: "key-test", kind: "generic", cwd: "/repo", status: "ready" }],
        },
        activeWorkspace: { [teamKey]: "key-test" },
      });
      const terminalCycle = vi
        .spyOn(useViewerStore.getState(), "cycleTerminalTab")
        .mockReturnValue(false);
      const cycle = vi.spyOn(useViewerStore.getState(), "cycleTab");
      try {
        await mountOnSettingsTab(rootOrder);
        fireEvent.keyDown(document, {
          key: "}",
          code: "BracketRight",
          metaKey: true,
          shiftKey: true,
        });
        expect(useViewerStore.getState().activeTab[teamKey]).toBe(NEIGHBOUR);
        expect(cycle).toHaveBeenCalledExactlyOnceWith(teamKey, "file", 1);
        expect(terminalCycle).not.toHaveBeenCalled();
      } finally {
        terminalCycle.mockRestore();
        cycle.mockRestore();
      }
    },
  );

  it.each(["first", "last"] as const)(
    "terminal capture cycles only the terminal with the root listener mounted %s",
    async (rootOrder) => {
      useViewerStore.setState({
        workspaces: {
          [teamKey]: [{ id: "key-test", kind: "generic", cwd: "/repo", status: "ready" }],
        },
        activeWorkspace: { [teamKey]: "key-test" },
      });
      const terminalCycle = vi
        .spyOn(useViewerStore.getState(), "cycleTerminalTab")
        .mockReturnValue(false);
      const cycle = vi.spyOn(useViewerStore.getState(), "cycleTab");
      const host = document.createElement("div");
      host.setAttribute("data-xterm-host", "");
      host.setAttribute("data-repo-path", "/repo");
      host.innerHTML = '<div class="xterm"><textarea></textarea></div>';
      document.body.appendChild(host);
      try {
        await mountOnSettingsTab(rootOrder);
        const field = host.querySelector("textarea")!;
        field.focus();
        const event = new KeyboardEvent("keydown", {
          key: "}",
          code: "BracketRight",
          metaKey: true,
          shiftKey: true,
          bubbles: true,
          cancelable: true,
        });
        field.dispatchEvent(event);
        expect(event.defaultPrevented).toBe(true);
        expect(terminalCycle).toHaveBeenCalledExactlyOnceWith(teamKey, "next");
        expect(cycle).not.toHaveBeenCalled();
        expect(useViewerStore.getState().activeTab[teamKey]).toBe(SETTINGS_VIRTUAL_PATH);
      } finally {
        host.remove();
        terminalCycle.mockRestore();
        cycle.mockRestore();
      }
    },
  );

  it("⌘⇧] moves from the Settings tab to its neighbour", async () => {
    await mountOnSettingsTab();
    act(() => {
      fireEvent.keyDown(document, {
        key: "}",
        code: "BracketRight",
        metaKey: true,
        shiftKey: true,
      });
    });
    expect(useViewerStore.getState().activeTab[teamKey]).toBe(NEIGHBOUR);
  });

  it("⌘` with the CLI closed reopens the terminal in its last mode", async () => {
    useViewerStore.getState().setLayoutMode("viewer");
    useViewerStore.setState({ lastTerminalMode: "shared" });
    await mountOnSettingsTab();
    expect(useViewerStore.getState().layoutMode).toBe("viewer");
    act(() => {
      fireEvent.keyDown(document, { key: "`", metaKey: true });
    });
    expect(useViewerStore.getState().layoutMode).toBe("shared");
  });

  it("⌘` focuses the shown team's viewer, not a retained hidden team's earlier in the page", async () => {
    useViewerStore.getState().setLayoutMode("shared");
    await mountOnSettingsTab();
    const hidden = document.createElement("div");
    hidden.style.display = "none";
    hidden.innerHTML = '<div data-viewer-content tabindex="-1" id="hidden-viewer"></div>';
    document.body.prepend(hidden);
    const terminal = document.createElement("div");
    terminal.className = "xterm";
    terminal.innerHTML = "<textarea></textarea>";
    document.body.appendChild(terminal);
    terminal.querySelector("textarea")!.focus();
    try {
      act(() => {
        fireEvent.keyDown(document, { key: "`", metaKey: true });
      });
      await act(async () => {
        await new Promise((r) => requestAnimationFrame(() => r(null)));
      });
      const focused = document.activeElement as HTMLElement | null;
      expect(focused?.id).not.toBe("hidden-viewer");
      expect(focused?.hasAttribute("data-viewer-content")).toBe(true);
    } finally {
      hidden.remove();
      terminal.remove();
    }
  });

  it("⌘` with focus in a row popup leaves the layout and the focused pane alone", async () => {
    useViewerStore.getState().setLayoutMode("viewer");
    useViewerStore.setState({ lastTerminalMode: "shared" });
    await mountOnSettingsTab();
    const popup = document.createElement("div");
    popup.setAttribute(ROW_POPUP_ATTR, "");
    popup.innerHTML = "<textarea></textarea>";
    document.body.appendChild(popup);
    const field = popup.querySelector("textarea")!;
    field.focus();
    try {
      act(() => {
        fireEvent.keyDown(field, { key: "`", metaKey: true });
      });
      await act(async () => {
        await new Promise((r) => requestAnimationFrame(() => r(null)));
      });
      expect(useViewerStore.getState().layoutMode).toBe("viewer");
      expect(document.activeElement).toBe(field);
    } finally {
      popup.remove();
    }
  });

  it("⌘F on the Settings tab shows the find bar and keeps Settings the active tab", async () => {
    await mountOnSettingsTab();
    expect(screen.queryByPlaceholderText("Find in document...")).toBeNull();
    act(() => {
      fireEvent.keyDown(document, { key: "f", metaKey: true });
    });
    expect(await screen.findByPlaceholderText("Find in document...")).toBeInTheDocument();
    expect(useViewerStore.getState().activeTab[teamKey]).toBe(SETTINGS_VIRTUAL_PATH);
  });

  it("⌘P on the Settings tab opens file quick-open", async () => {
    await mountOnSettingsTab("first");
    expect(screen.queryByPlaceholderText("Search files...")).toBeNull();
    act(() => {
      fireEvent.keyDown(document, { key: "p", metaKey: true });
    });
    expect(await screen.findByPlaceholderText("Search files...")).toBeInTheDocument();
  });
});
