// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { openMessageTab, messageTabPath } from "../components/inbox-message-dashboard";
import { isLiveInboxMessage } from "./live-inbox-message";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

vi.mock("@/features/dashboard/components/html-viewer", () => ({
  HtmlViewer: () => null,
  messageDocKey: () => "",
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
}));

describe("live inbox message eligibility", () => {
  it.each([
    ["human", "H1", "html", true],
    ["human", "H1", "markdown", false],
    ["human", "H1", "plain", false],
    ["human", "human", "html", false],
    ["H2", "H1", "html", false],
    [null, "H1", "html", false],
  ] as const)("%s from %s as %s is live: %s", (to, from, contentType, expected) => {
    expect(isLiveInboxMessage({ to, from, contentType })).toBe(expected);
  });
});

it("opens a message through management tabs once and focuses it again without changing layout", () => {
  const team = { org: "acme", team: "web" };
  const key = "acme:web";
  useOrgStore.setState({ activeOrgSlug: team.org, activeTeamName: team.team });
  useViewerStore.setState({
    openTabs: {},
    activeTab: {},
    openedTeams: [],
    cliExpanded: true,
    stateLoaded: false,
  });
  openMessageTab(team, 42, "H1", { keepLayout: true });
  useViewerStore.setState({ activeTab: { [key]: "elsewhere" } });
  openMessageTab(team, 42, "H1", { keepLayout: true });
  const state = useViewerStore.getState();
  expect(state.openTabs[key]).toHaveLength(1);
  expect(state.activeTab[key]).toBe(messageTabPath(team, 42, "H1"));
  expect(state.cliExpanded).toBe(true);
});
