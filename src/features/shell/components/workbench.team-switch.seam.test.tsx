// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import { useEffect } from "react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { installMockBackend } from "@/app/mock/install";
import { MOCK_ORG_SLUG } from "@/app/mock/fixtures";
import { expectFlatAcross } from "@/shared/lib/warm-scopes/__test-utils__/expect-flat-across";
import { buildTeamKey } from "@/shared/lib/team-key";
import { applyActiveTeamSwitch } from "@/features/org/hooks/use-org-registry-sync";
import { switchContext, switchContextToTeamName } from "@/features/viewer/lib/context-switch";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import type { Org } from "@/shared/types/domain";
import { registerBuiltinPanels } from "@/features/shell/right-pane/builtin-panels";
import { useTeamSurfaceStore } from "@/features/viewer/lib/team-surface-store";
import { Workbench } from "./workbench";

// What the switch is judged by. The team views and the terminal tiles stand in as boxes that count
// their own mounts, so the test sees exactly what the workbench mounts, hides and unmounts.
const counts = vi.hoisted(() => ({
  teamViewMounts: 0,
  teamViewUnmounts: 0,
  emptyStateRenders: 0,
  panelsMounts: 0,
  panelsUnmounts: 0,
  uncommittedActive: 0,
}));

vi.mock("@/features/viewer/views/team-workspace-view", () => ({
  TeamWorkspaceView: function TeamViewProbe({ teamKey }: { teamKey: string }) {
    useEffect(() => {
      counts.teamViewMounts += 1;
      return () => void (counts.teamViewUnmounts += 1);
    }, []);
    return <div data-team-view={teamKey} />;
  },
}));
vi.mock("@/features/dashboard/components/team-workspace-empty-state", () => ({
  TeamWorkspaceEmptyState: function EmptyProbe() {
    counts.emptyStateRenders += 1;
    return <div data-testid="empty-state" />;
  },
}));
vi.mock("@/features/viewer/components/workspace-terminal-panels", () => ({
  WorkspaceTerminalPanels: function PanelsProbe({ teamKey }: { teamKey: string }) {
    useEffect(() => {
      counts.panelsMounts += 1;
      return () => void (counts.panelsUnmounts += 1);
    }, []);
    return <div data-testid="cli-panels" data-team={teamKey} />;
  },
  WorkspaceInitOverlay: () => null,
}));

const ORG2 = "zed/other";
const A = buildTeamKey(MOCK_ORG_SLUG, "a");
const B = buildTeamKey(MOCK_ORG_SLUG, "b");
const COLD = buildTeamKey(MOCK_ORG_SLUG, "cold");
const C = buildTeamKey(ORG2, "c");

const story = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });

function org(slug: string, teams: string[]): Org {
  return {
    slug,
    display_name: slug,
    created_at: "2026-01-01T00:00:00Z",
    org_root: `/fake/${slug}`,
    remote_url: null,
    config_flags: {},
    teams: teams.map((name) => ({ name, source_repos: [] })),
    people: [],
  };
}

/** Records every committed render in which the active team is not in `openedTeams`: the commit that
 *  used to collapse the CLI into the empty state. */
function ActiveTeamOpenSpy() {
  const orgSlug = useOrgStore((s) => s.activeOrgSlug);
  const team = useOrgStore((s) => s.activeTeamName);
  const opened = useViewerStore((s) => s.openedTeams);
  const uncommitted =
    orgSlug !== null && team !== null && !opened.includes(buildTeamKey(orgSlug, team));
  useEffect(() => {
    if (uncommitted) counts.uncommittedActive += 1;
  });
  return null;
}

/** The CLI's terminal section as a user sees it: collapsed to nothing is the flash. */
const terminalCollapsed = () =>
  document.querySelector<HTMLElement>("[data-testid='terminal-section']")?.style.flex === "0 0 0px";
let collapsedRenders = 0;
let rightColumnFlexChanges = 0;
let stopWatching: (() => void) | null = null;

/** The right column's width as set on it: a flip between the docked width and 0 is the jump. */
let lastRightColumnFlex: string | null = null;
function watchTerminalSection() {
  lastRightColumnFlex = null;
  const observer = new MutationObserver(() => {
    if (terminalCollapsed()) collapsedRenders += 1;
    const flex = document.querySelector<HTMLElement>("[data-testid='right-pane-column']")?.style
      .flex;
    if (flex !== undefined && lastRightColumnFlex !== null && flex !== lastRightColumnFlex)
      rightColumnFlexChanges += 1;
    if (flex !== undefined) lastRightColumnFlex = flex;
  });
  observer.observe(document.body, { subtree: true, attributes: true, childList: true });
  stopWatching = () => observer.disconnect();
}

async function mount() {
  render(
    <TooltipProvider>
      <ActiveTeamOpenSpy />
      <Workbench />
    </TooltipProvider>,
  );
  await act(async () => {});
}

const settle = () => act(async () => {});

beforeEach(() => {
  Object.assign(counts, {
    teamViewMounts: 0,
    teamViewUnmounts: 0,
    emptyStateRenders: 0,
    panelsMounts: 0,
    panelsUnmounts: 0,
    uncommittedActive: 0,
  });
  collapsedRenders = 0;
  rightColumnFlexChanges = 0;
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  installMockBackend();
  registerBuiltinPanels();
  useTeamSurfaceStore.setState({ trees: {} });
  useOrgStore.setState({
    orgs: [org(MOCK_ORG_SLUG, ["a", "b", "cold"]), org(ORG2, ["c"])],
    activeOrgSlug: MOCK_ORG_SLUG,
    activeTeamName: "a",
    lastTeamByOrgSlug: {},
    shownTeamsByOrgSlug: {},
    disabledOrgSlugs: [],
    hasUserIntent: true,
  });
  useViewerStore.setState({
    openedTeams: [A, B],
    workspaces: {
      [A]: [story("a-1")],
      [B]: [story("b-1")],
      [COLD]: [story("cold-1")],
      [C]: [story("c-1")],
    },
    activeWorkspace: { [A]: "a-1", [B]: "b-1", [COLD]: "cold-1", [C]: "c-1" },
    layoutMode: "shared",
    rightPaneOpen: true,
    terminalVisible: true,
    workspaceTraySeenCoachmark: true,
  });
});

afterEach(() => {
  stopWatching?.();
  stopWatching = null;
  cleanup();
  document.body.innerHTML = "";
  useUiStore.setState({ consoleSwitcherSlot: null });
  useViewerStore.setState({ openedTeams: [], workspaces: {}, activeWorkspace: {} });
  useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

describe("a team switch never commits an active team that is not open (AC2)", () => {
  const paths: Array<[string, () => void | Promise<void>]> = [
    ["an agent click", () => void switchContext({ teamKey: COLD }, "agent")],
    ["the team picker", () => void switchContextToTeamName("cold", "switcher")],
    ["the MCP", () => applyActiveTeamSwitch({ orgSlug: MOCK_ORG_SLUG, team: "cold" } as never)],
    ["a cross-org switch to a team never opened", () => void switchContext({ teamKey: C }, "nav")],
  ];

  for (const [name, go] of paths) {
    it(`${name}: no commit shows the empty state, collapses the CLI, or sees an unopened active team`, async () => {
      await mount();
      watchTerminalSection();
      counts.emptyStateRenders = 0;
      counts.uncommittedActive = 0;
      useViewerStore.setState({ openedTeams: [A] });
      await settle();
      await act(async () => {
        await go();
      });
      await settle();
      expect(counts.uncommittedActive).toBe(0);
      expect(counts.emptyStateRenders).toBe(0);
      expect(collapsedRenders).toBe(0);
      expect(counts.panelsUnmounts).toBe(0);
      expect(document.querySelector("[data-testid='cli-panels']")).not.toBeNull();
      expect(document.querySelector("[data-workspace-bar]")).not.toBeNull();
    });
  }
});

// 100 full-workbench switches in happy-dom take ~5 s: this is the test's own budget, not a bump.
const FIFTY_ROUND_TRIPS_MS = 30_000;

describe("flat across 50 round trips", () => {
  const read = () => ({
    teamViewMounts: counts.teamViewMounts,
    teamViewUnmounts: counts.teamViewUnmounts,
    emptyStateRenders: counts.emptyStateRenders,
    panelsMounts: counts.panelsMounts,
    panelsUnmounts: counts.panelsUnmounts,
    uncommittedActive: counts.uncommittedActive,
    collapsedRenders,
    rightColumnFlexChanges,
  });

  it(
    "A<->B in one org",
    async () => {
      await mount();
      watchTerminalSection();
      await expectFlatAcross(async () => {
        await act(async () => {
          switchContext({ teamKey: B }, "agent");
          await Promise.resolve();
          switchContext({ teamKey: A }, "agent");
        });
      }, read);
    },
    FIFTY_ROUND_TRIPS_MS,
  );

  it(
    "A<->C across orgs",
    async () => {
      await mount();
      watchTerminalSection();
      await expectFlatAcross(async () => {
        await act(async () => {
          switchContext({ teamKey: C }, "agent");
          await Promise.resolve();
          switchContext({ teamKey: A }, "agent");
        });
      }, read);
      expect(counts.emptyStateRenders).toBe(0);
    },
    FIFTY_ROUND_TRIPS_MS,
  );

  it(
    "A<->B in separate commits: each destination is the shown CLI team",
    async () => {
      await mount();
      watchTerminalSection();
      const shownCliTeams = () =>
        [...document.querySelectorAll<HTMLElement>("[data-cli-team]")]
          .filter((el) => el.style.display !== "none")
          .map((el) => el.getAttribute("data-cli-team"));
      await settle();
      await expectFlatAcross(async () => {
        await act(async () => void switchContext({ teamKey: B }, "agent"));
        expect(shownCliTeams()).toEqual([B]);
        await act(async () => void switchContext({ teamKey: A }, "agent"));
        expect(shownCliTeams()).toEqual([A]);
      }, read);
    },
    FIFTY_ROUND_TRIPS_MS,
  );
});
