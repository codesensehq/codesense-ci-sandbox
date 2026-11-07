// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, onTestFinished, vi } from "vitest";
import { act, cleanup, render, waitFor } from "@testing-library/react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { installMockBackend } from "@/app/mock/install";
import { MOCK_ORG_SLUG } from "@/app/mock/fixtures";

import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { attachRightPaneHost } from "@/test/right-pane-host";

import { TeamWorkspaceView } from "./team-workspace-view";

type Internals = {
  invoke: (cmd: string, args?: Record<string, unknown>, options?: unknown) => Promise<unknown>;
};

/**
 * The team view mounted over the mock backend, through the same
 * `window.__TAURI_INTERNALS__` door the app uses. A team whose skills and
 * commands sit flat in `content/` renders as it is: nothing classifies or
 * migrates them on mount.
 */
describe("TeamWorkspaceView on a flat-payload team", () => {
  const invoked: string[] = [];

  // The team view draws its right pane (which reads the notes) into the workbench's host.
  let releaseHost: () => void = () => {};
  beforeEach(() => {
    releaseHost = attachRightPaneHost(`${MOCK_ORG_SLUG}:default`);
    invoked.length = 0;
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    installMockBackend();
    const internals = (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__;
    const invoke = internals.invoke.bind(internals);
    internals.invoke = (cmd, args, options) => {
      invoked.push(cmd);
      return invoke(cmd, args, options);
    };
  });

  afterEach(() => {
    cleanup();
    releaseHost();
    useViewerStore.setState({
      layoutMode: null,
      terminalVisible: false,
      lastTerminalMode: "shared",
    });
    useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  it("mounts without calling any pack migration command", async () => {
    const { container } = render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={`${MOCK_ORG_SLUG}:default`} />
      </TooltipProvider>,
    );
    // The team renders and its mount effects run (its agents and notes are read).
    await waitFor(() =>
      expect(invoked).toEqual(expect.arrayContaining(["agents_list", "list_notes"])),
    );
    expect(container.textContent).not.toBe("");
    expect(invoked.filter((cmd) => /migrat/i.test(cmd))).toEqual([]);
  });

  it("shows the active team's files as plain content: nothing resolves pack provenance or overrides", async () => {
    useOrgStore.setState({ activeOrgSlug: MOCK_ORG_SLUG, activeTeamName: "default" });
    const { container } = render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={`${MOCK_ORG_SLUG}:default`} />
      </TooltipProvider>,
    );
    await waitFor(() =>
      expect(invoked).toEqual(expect.arrayContaining(["agents_list", "list_notes"])),
    );
    expect(invoked.filter((cmd) => /provenance|override|fork|baseline/i.test(cmd))).toEqual([]);
    expect(container.querySelector("[data-testid^='pack-provenance']")).toBeNull();
    expect(container.querySelector("[data-testid^='pack-override']")).toBeNull();
  });

  it("focusing the active workspace's doc on activation keeps full-screen CLI on", async () => {
    const teamKey = `${MOCK_ORG_SLUG}:default`;
    const story = (id: string) => ({
      id,
      kind: "story" as const,
      cwd: `/wt/${id}`,
      storyFilePath: `/stories/${id}.md`,
      storyId: id,
      status: "ready" as const,
    });
    const openStoryDocForTeam = vi.fn();
    const before = useViewerStore.getState();
    onTestFinished(() =>
      useViewerStore.setState({
        openStoryDocForTeam: before.openStoryDocForTeam,
        settings: before.settings,
        workspaces: before.workspaces,
        activeWorkspace: before.activeWorkspace,
      }),
    );
    useViewerStore.setState((s) => ({
      openStoryDocForTeam,
      settings: { ...s.settings, focusWorkspaceDocOnActivation: true },
      workspaces: { ...s.workspaces, [teamKey]: [story("S-1"), story("S-2")] },
      activeWorkspace: { ...s.activeWorkspace, [teamKey]: "S-1" },
    }));
    useViewerStore.getState().setLayoutMode("console");
    render(
      <TooltipProvider>
        <TeamWorkspaceView teamKey={teamKey} />
      </TooltipProvider>,
    );
    act(() => {
      useViewerStore.setState((s) => ({
        activeWorkspace: { ...s.activeWorkspace, [teamKey]: "S-2" },
      }));
    });
    await waitFor(() =>
      expect(openStoryDocForTeam).toHaveBeenCalledWith(teamKey, "/stories/S-2.md", {
        keepLayout: true,
      }),
    );
  });
});
