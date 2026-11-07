// SPDX-License-Identifier: MIT
import { act, cleanup, render } from "@testing-library/react";
import { useEffect } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Seam: the REAL dock host, single-slot resolution, dashboard-profile store, slot choice reader,
// org store and warm-scope owner. The page host and the built-in list are probes that count their
// own mounts, so the test sees exactly what the dock builds, keeps hidden and shows. The profile
// topic's answer is applied the way the configuration connection does.
const counts = vi.hoisted(() => ({ pageMounts: 0, pageUnmounts: 0, nativeRenders: 0 }));
vi.mock("./slot-dashboard-host", () => ({
  SlotDashboardHost: function PageProbe({
    entry,
    visible,
  }: {
    entry: { id: string };
    visible: boolean;
  }) {
    useEffect(() => {
      counts.pageMounts += 1;
      return () => void (counts.pageUnmounts += 1);
    }, []);
    return <div data-testid="dock-page" data-page={entry.id} data-visible={String(visible)} />;
  },
}));
vi.mock("@/features/viewer/components/agents-list", () => ({
  AgentsList: function NativeProbe() {
    counts.nativeRenders += 1;
    return <div data-testid="native-agents" />;
  },
}));
vi.mock("@/features/viewer/lib/agents-list-focus", async (original) => ({
  ...(await original<typeof import("@/features/viewer/lib/agents-list-focus")>()),
  takeAgentsFocus: () => false,
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  listBuiltinPanes: vi.fn(async () => ({ contentRoot: "/x", chain: [], panes: [] })),
}));

import { TooltipProvider } from "@/components/ui/tooltip";
import { expectFlatAcross } from "@/shared/lib/warm-scopes/__test-utils__/expect-flat-across";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  followOrgProfile,
  resetProfiles,
  useDashboardProfileStore,
} from "@/features/dashboard/lib/dashboard-profile";
import { dashboardRefKey } from "@/features/dashboard/lib/dashboard-ref";
import { usePaneStore } from "../right-pane/panes/pane-store";
import { DockSlotHost } from "./dock-slot-host";
import type { DashboardProfile, DashboardRef, PaneEntry } from "@/tauri/commands";

const ORG_A = "acme";
const ORG_C = "other";
const A = buildTeamKey(ORG_A, "a");
const C = buildTeamKey(ORG_C, "c");

const refFor = (org: string): DashboardRef => ({ scope: "team", org, team: "t", id: "people" });
const entryFor = (org: string): PaneEntry => ({
  dashboardRef: refFor(org),
  id: `people-${org}`,
  scope: "team",
  team: "t",
  title: "People",
  path: `/ctx/${org}/panes/people.html`,
  panesDir: `/ctx/${org}/panes`,
  panelId: `pane:${org}:t/people`,
});
const profileFor = (org: string): DashboardProfile =>
  ({
    id: "default",
    name: "Default",
    posture: "",
    slots: { dock: refFor(org) },
    cliLayout: "default",
    chrome: {},
  }) as DashboardProfile;

function Dock() {
  const org = useOrgStore((s) => s.activeOrgSlug);
  const team = useOrgStore((s) => s.activeTeamName);
  return <DockSlotHost teamKey={org && team ? buildTeamKey(org, team) : A} visible />;
}

/** What the configuration connection does on an org switch: follow the org, then answer its topic. */
function switchOrg(org: string, team: string) {
  useOrgStore.setState({ activeOrgSlug: org, activeTeamName: team });
  warmScopes.activate(buildTeamKey(org, team));
  followOrgProfile(org);
}
const answerProfile = (org: string) =>
  useDashboardProfileStore.setState({ profile: profileFor(org) });

beforeEach(() => {
  Object.assign(counts, { pageMounts: 0, pageUnmounts: 0, nativeRenders: 0 });
  resetProfiles();
  for (const key of [A, C]) warmScopes.forget(key);
  useOrgStore.setState({
    orgs: [
      { slug: ORG_A, teams: [{ name: "a" }] },
      { slug: ORG_C, teams: [{ name: "c" }] },
    ] as never,
    activeOrgSlug: ORG_A,
    activeTeamName: "a",
  });
  warmScopes.activate(A);
  const rows = Object.fromEntries(
    [ORG_A, ORG_C].map((org) => [
      dashboardRefKey(refFor(org)),
      {
        id: dashboardRefKey(refFor(org)),
        dashboardRef: refFor(org),
        entry: entryFor(org),
        contentRoot: `/ctx/${org}/content`,
        sourceRevision: 0,
        documentRevision: 0,
        listed: true,
      },
    ]),
  );
  usePaneStore
    .getState()
    .setListing(
      A,
      { contentRoot: `/ctx/${ORG_A}/content`, chain: ["a"], panes: [entryFor(ORG_A)] },
      rows,
    );
  followOrgProfile(ORG_A);
  answerProfile(ORG_A);
});

afterEach(() => {
  cleanup();
  resetProfiles();
});

const dockPages = () =>
  [...document.querySelectorAll<HTMLElement>("[data-testid='dock-page']")].map((el) => ({
    id: el.dataset.page,
    visible: el.dataset.visible === "true",
  }));

describe("the dock across orgs (AC8)", () => {
  it("keeps one page per warm org, hidden, and shows each org's choice in the first frame", async () => {
    render(
      <TooltipProvider>
        <Dock />
      </TooltipProvider>,
    );
    await act(async () => {});
    expect(dockPages()).toEqual([{ id: `people-${ORG_A}`, visible: true }]);
    await act(async () => switchOrg(ORG_C, "c"));
    // A first visit to a cold org shows the list until its profile topic answers, as on main.
    await act(async () => answerProfile(ORG_C));
    const nativeAfterColdVisit = counts.nativeRenders;
    expect(dockPages()).toEqual([
      { id: `people-${ORG_A}`, visible: false },
      { id: `people-${ORG_C}`, visible: true },
    ]);
    await act(async () => switchOrg(ORG_A, "a"));
    // Org A's profile is warm: its page is the shown one in this very commit, nothing rebuilt.
    expect(dockPages()).toEqual([
      { id: `people-${ORG_A}`, visible: true },
      { id: `people-${ORG_C}`, visible: false },
    ]);
    expect(counts.nativeRenders).toBe(nativeAfterColdVisit);
    expect(counts.pageMounts).toBe(2);
    expect(counts.pageUnmounts).toBe(0);
  });

  it("a warm org's dock page stays shown while the pane store still projects the other org's sources", async () => {
    render(
      <TooltipProvider>
        <Dock />
      </TooltipProvider>,
    );
    await act(async () => {});
    await act(async () => switchOrg(ORG_C, "c"));
    await act(async () => answerProfile(ORG_C));
    const rows = usePaneStore.getState().sourceRows;
    // Returning to A: the store holds only C's view until A's listing lands.
    usePaneStore.setState({
      teamKey: C,
      sourceRows: { [dashboardRefKey(refFor(ORG_C))]: rows[dashboardRefKey(refFor(ORG_C))] },
    });
    await act(async () => switchOrg(ORG_A, "a"));
    expect(dockPages()).toEqual([
      { id: `people-${ORG_A}`, visible: true },
      { id: `people-${ORG_C}`, visible: false },
    ]);
    expect(document.querySelector("[data-testid='native-agents']")).toBeNull();
  });

  it("A<->C across orgs builds and shows the list zero times once both orgs have been shown", async () => {
    render(
      <TooltipProvider>
        <Dock />
      </TooltipProvider>,
    );
    await act(async () => {});
    await expectFlatAcross(
      async () => {
        await act(async () => switchOrg(ORG_C, "c"));
        await act(async () => answerProfile(ORG_C));
        await act(async () => switchOrg(ORG_A, "a"));
        await act(async () => answerProfile(ORG_A));
      },
      () => ({ ...counts }),
    );
  }, 30_000);
});
