// SPDX-License-Identifier: MIT
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { TooltipProvider } from "@/components/ui/tooltip";
import { installMockBackend } from "@/app/mock/install";
import { useOrgStore } from "@/shared/stores/org-store";
import { useConfigStore } from "@/shared/stores/config-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { openFileInTeam } from "@/shared/lib/open-file-in-team";
import type { CommonViewerProps } from "@/shared/lib/viewer-file-type";
import type { Org } from "@/shared/types/domain";
import { runShowPlan } from "../lib/show-agent";
import { TeamWorkspaceView } from "./team-workspace-view";

// The real mounted view owns navigation; only the page renderer is replaced
// with a control that sends the same automatic open options as the HTML bridge.
vi.mock("@/shared/lib/render-viewer", () => ({
  renderViewer: (props: CommonViewerProps) => (
    <button
      data-testid="script-open-preview"
      onClick={() =>
        openFileInTeam(
          { org: "acme", team: "default", path: "/orgs/acme/default/content/auto.md" },
          { keepLayout: true, navigate: props.onNavigate },
        )
      }
    >
      Run page script
    </button>
  ),
}));
vi.mock("@/features/viewer/lib/focus-cascade", async (original) => ({
  ...(await original<typeof import("@/features/viewer/lib/focus-cascade")>()),
  focusActiveWorkspaceTerminal: vi.fn(),
}));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

it("an automatic same-team preview through the mounted view preserves a pending Show", async () => {
  installMockBackend();
  const teamKey = "acme:default";
  const initial = "/orgs/acme/default/content/page.html";
  const org: Org = {
    slug: "acme",
    display_name: "Acme",
    created_at: "",
    remote_url: null,
    config_flags: {},
    people: [],
    org_root: "/orgs/acme",
    teams: [{ name: "default", source_repos: [] }],
  };
  useOrgStore.setState({
    orgs: [org],
    activeOrgSlug: "acme",
    activeTeamName: "default",
    workspaceRepos: {},
  });
  useConfigStore.setState({ initialized: true, repos: [] });
  useViewerStore.setState({
    stateLoaded: true,
    scopeRecoveryComplete: true,
    openedTeams: [teamKey],
    openTabs: { [teamKey]: [{ path: initial, name: "page" }] },
    activeTab: { [teamKey]: initial },
    workspaces: { [teamKey]: [{ id: "agent", kind: "generic", cwd: "/agent", status: "ready" }] },
    activeWorkspace: { [teamKey]: "agent" },
    softClosedWorkspaces: {},
    layoutMode: "shared",
    terminalVisible: true,
    rightPaneOpen: false,
    teamPipelineRepoPath: {},
    settings: { ...useViewerStore.getState().settings, focusWorkspaceDocOnActivation: false },
  });
  render(
    <TooltipProvider>
      <TeamWorkspaceView teamKey={teamKey} />
    </TooltipProvider>,
  );
  const script = await screen.findByTestId("script-open-preview");
  let finish!: () => void;
  const gate = new Promise<void>((resolve) => {
    finish = resolve;
  });
  const adopt = vi
    .spyOn(useViewerStore.getState(), "adoptTerminalSession")
    .mockImplementation(() => gate);
  try {
    const pending = runShowPlan(
      {
        kind: "adopt",
        teamKey,
        sessionId: "slow-agent",
        storyId: null,
        tabDir: null,
        dir: null,
        confirm: false,
        ended: { runId: null, storyId: null },
      },
      { handle: "slow-agent" },
      { teamKey, name: "Agent" },
    );
    expect(adopt).toHaveBeenCalledOnce();
    fireEvent.click(script);
    expect(useViewerStore.getState().activeTab[teamKey]).toBe("/orgs/acme/default/content/auto.md");
    await act(async () => {
      finish();
      expect(await pending).toEqual({ shown: "adopted" });
    });
  } finally {
    finish();
  }
});
