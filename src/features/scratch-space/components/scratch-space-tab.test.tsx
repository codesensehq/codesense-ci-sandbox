// SPDX-License-Identifier: MIT
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { listing as paneListing } from "@/features/shell/right-pane/panes/pane-seam-helpers";
import {
  defaultRightPaneLayout,
  SCRATCH_PANEL_ID,
} from "@/features/shell/right-pane/right-pane-layout";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import { useScratchTeamUseStore } from "@/shared/stores/scratch-team-use-store";
import { ScratchSpaceTab } from "./scratch-space-tab";
const { listing, revealPath } = vi.hoisted(() => ({ listing: vi.fn(), revealPath: vi.fn() }));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  listScratchSpace: listing,
  revealPath,
  listNotes: async () => [{ id: "1", title: "My secret note" }],
  routineList: async () => [],
  agentsList: async () => [],
  getScratchTeamUse: async () => true,
  saveAppState: async () => {},
  loadAppState: async () => "{}",
  pushViewerState: async () => {},
}));
vi.mock("./scratch-item-actions", () => ({ ScratchItemMenu: () => <button>Item actions</button> }));
const contents = {
  orgSlug: "acme/ctx",
  exists: true,
  files: [{ path: "/s/idea.md", logical: "idea.md", inertKind: null }],
  allOrgs: [
    { path: "/s/all-orgs/prompts/review.md", logical: "prompts/review.md", kind: "prompt" },
  ],
  syncedOrgs: ["acme/ctx"],
  management: {
    repo: { path: "/private/repo", lastCommitAt: 100, uncommitted: 3, error: null },
    orgs: [
      {
        org: "acme/ctx",
        exists: true,
        counts: { notes: 1, overlayFiles: 1, routines: 0, agents: 0, panes: 0 },
        offTeams: [],
        settingsDamaged: false,
        uncommitted: 3,
        tracked: true,
      },
    ],
  },
};
beforeEach(() => {
  vi.clearAllMocks();
  listing.mockResolvedValue(contents);
  useOrgStore.setState({
    orgs: [
      {
        slug: "acme/ctx",
        display_name: "Acme",
        org_root: "/ctx",
        teams: [{ name: "dev", source_repos: [] }],
      } as never,
    ],
    activeOrgSlug: "acme/ctx",
    activeTeamName: "dev",
  });
  useViewerStore.setState({
    navLens: {},
    navCollapsed: { "acme/ctx:dev": true },
    stateLoaded: true,
  });
  useLocalProfileStore.setState({
    initialized: true,
    status: {
      active: { principal: "42", displayName: "Me", machineOwner: null },
      profiles: [],
      privateAuthority: null,
    },
  });
  usePaneStore.setState({ listing: null });
  useScratchTeamUseStore.setState({ byTeam: {}, saving: {} });
});
const mount = () => render(<ScratchSpaceTab teamContentPath="" filePath="virtual://scratch" />);
it("shows repo management, switches, summaries and all-orgs items without browsing notes or files", async () => {
  mount();
  const repo = await screen.findByRole("region", { name: "Your private repo" });
  expect(repo).toHaveTextContent("/private/repo");
  expect(repo).toHaveTextContent("3 uncommitted edits");
  fireEvent.click(within(repo).getByRole("button", { name: "Reveal" }));
  expect(revealPath).toHaveBeenCalledWith("/private/repo");
  expect(
    await screen.findByRole("switch", { name: "Use my private items in dev" }),
  ).toBeInTheDocument();
  expect(screen.getByRole("button", { name: /Open notes/ })).toBeInTheDocument();
  expect(screen.getByRole("region", { name: "All orgs" })).toHaveTextContent("review");
  expect(screen.queryByRole("region", { name: "Notes" })).toBeNull();
  expect(screen.queryByRole("region", { name: "Files" })).toBeNull();
  expect(screen.queryByText("My secret note")).toBeNull();
  expect(screen.queryByText("idea.md")).toBeNull();
  expect(screen.queryByRole("button", { name: "New note" })).toBeNull();
});
it("Show in left nav selects private for the active team and reveals it", async () => {
  mount();
  await screen.findByRole("region", { name: "Your private repo" });
  fireEvent.click(screen.getByRole("button", { name: "Show in left nav" }));
  expect(useViewerStore.getState().navLens["acme/ctx:dev"].source).toEqual({ kind: "private" });
  expect(useViewerStore.getState().navCollapsed["acme/ctx:dev"]).toBe(false);
});
it("shows a read failure with retry", async () => {
  listing.mockRejectedValueOnce(new Error("disk unavailable"));
  mount();
  expect(await screen.findByTestId("scratch-space-error")).toHaveTextContent("disk unavailable");
  listing.mockResolvedValue(contents);
  fireEvent.click(screen.getByRole("button", { name: "Try again" }));
  await screen.findByRole("region", { name: "Your private repo" });
});
it("requires an active team to show the nav", async () => {
  useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
  mount();
  expect(screen.getByRole("button", { name: "Show in left nav" })).toBeDisabled();
  await waitFor(() => expect(screen.getByTestId("scratch-space-no-org")).toBeInTheDocument());
});
it("disables Show in left nav for an org with no team selected", async () => {
  useOrgStore.setState({ activeTeamName: null });
  mount();
  const button = screen.getByRole("button", { name: "Show in left nav" });
  expect(button).toBeDisabled();
  expect(screen.getByText("Open a team first.")).toBeVisible();
  fireEvent.click(button);
  expect(useViewerStore.getState().navLens["acme/ctx:_none"]).toBeUndefined();
  await screen.findByRole("region", { name: "Your private repo" });
});
it("explains the missing profile", () => {
  useLocalProfileStore.setState({ status: { active: null, profiles: [], privateAuthority: null } });
  mount();
  expect(screen.getByText("Sign in to a profile to have a private space.")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Show in left nav" })).toBeDisabled();
});

it.each([false, true])(
  "Open notes reveals the selected Notes surface (dashboard: %s)",
  async (dashboard) => {
    const panes = paneListing({
      org: "acme/ctx",
      team: "dev",
      root: "/ctx",
      id: "notes",
      title: "Notes",
      privateCopy: true,
      surface: "notes",
    });
    usePaneStore.setState({ teamKey: "acme/ctx:dev", listing: panes, knownEntries: {} });
    useDashboardProfileStore.setState({
      org: "acme/ctx",
      profile: {
        id: "default",
        name: "Default",
        posture: "",
        slots: { "right-pane": dashboard ? [panes.panes[0].dashboardRef!] : "native" },
        cliLayout: "default",
        chrome: {},
      },
    });
    useViewerStore.setState({ rightPaneOpen: false, rightPaneLayout: defaultRightPaneLayout() });
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Open notes" }));
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
    expect(
      useViewerStore
        .getState()
        .rightPaneLayout.sections.some(
          (s) =>
            !s.collapsed &&
            s.group.activePanelId === (dashboard ? panes.panes[0].panelId : SCRATCH_PANEL_ID),
        ),
    ).toBe(true);
  },
);
