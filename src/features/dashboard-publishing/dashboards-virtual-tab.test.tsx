// SPDX-License-Identifier: MIT
import { afterEach, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { __resetVirtualTabsForTest, resolveVirtualTab } from "@/shared/lib/virtual-viewer-registry";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { NoTeamTabHost } from "@/features/shell/components/no-team-tab-host";
import {
  dashboardPublishingFallback,
  openDashboards,
  registerDashboardsTab,
  DASHBOARDS_VIRTUAL_PATH,
} from "./dashboards-virtual-tab";
vi.mock("./dashboards-tab", () => ({ DashboardsTab: () => <div>Publishing surface</div> }));
afterEach(() => {
  delete window.__CODESENSE_DASHBOARD_PUBLISH__;
  __resetVirtualTabsForTest();
  useUiStore.setState({ noTeamTabs: [], noTeamActiveTab: null });
});
it("registers and focuses a single real management tab with no team", () => {
  window.__CODESENSE_DASHBOARD_PUBLISH__ = true;
  useOrgStore.setState({ activeOrgSlug: null });
  openDashboards();
  openDashboards();
  expect(useUiStore.getState().noTeamTabs).toEqual([DASHBOARDS_VIRTUAL_PATH]);
  render(<NoTeamTabHost>Home</NoTeamTabHost>);
  expect(screen.getByText("Publishing surface")).toBeInTheDocument();
});
it("keeps the registration absent and lets a restored feature-off tab close", () => {
  window.__CODESENSE_DASHBOARD_PUBLISH__ = false;
  registerDashboardsTab();
  expect(resolveVirtualTab(DASHBOARDS_VIRTUAL_PATH)).toBeNull();
  expect(dashboardPublishingFallback(DASHBOARDS_VIRTUAL_PATH)).not.toBeNull();
  useOrgStore.setState({ activeOrgSlug: null });
  useUiStore.setState({
    noTeamTabs: [DASHBOARDS_VIRTUAL_PATH],
    noTeamActiveTab: DASHBOARDS_VIRTUAL_PATH,
  });
  render(<NoTeamTabHost>Home</NoTeamTabHost>);
  expect(screen.getByText("Dashboard publishing isn't part of this build.")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Close tab" }));
  expect(screen.getByText("Home")).toBeInTheDocument();
});
