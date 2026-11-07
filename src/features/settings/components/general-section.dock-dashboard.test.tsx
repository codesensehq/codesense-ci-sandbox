// SPDX-License-Identifier: MIT
/**
 * SV-PLG-8: the "Agents list" row resolves a saved dock choice from its own team, as the dock
 * does, so a team-A dashboard is not shown as missing while sibling team B is active.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

const { listPanes, writeDashboardProfile } = vi.hoisted(() => ({
  listPanes: vi.fn(),
  writeDashboardProfile: vi.fn(async (_org, profile) => profile),
}));
vi.mock("next-themes", () => ({
  useTheme: () => ({ theme: "system", setTheme: vi.fn() }),
}));
vi.mock("@/tauri/commands", () => ({
  detectTools: vi.fn().mockResolvedValue({ editors: [], terminals: [] }),
  checkForUpdate: vi.fn().mockResolvedValue(null),
  listPanes,
  writeDashboardProfile,
}));
vi.mock("@tauri-apps/api/app", () => ({
  getVersion: vi.fn().mockResolvedValue("0.0.0-test"),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@/app/update-toast", () => ({ runUpdateCheck: vi.fn() }));

import { GeneralSection } from "./general-section";
import { useOrgStore } from "@/shared/stores/org-store";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";

const ref = { scope: "team" as const, org: "acme", team: "a", id: "agents" };
const entry = {
  id: "agents",
  title: "Team agents",
  scope: "team" as const,
  team: "a",
  path: "/ctx/a/panes/agents.html",
  panesDir: "/ctx/a/panes",
  panelId: "pane:acme:a/agents",
  dashboardRef: ref,
  declaration: { slots: ["dock"], capabilities: [] },
};
beforeEach(() => {
  listPanes.mockReset();
  // Sibling team B is active; its listing does not hold A's dashboard.
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "b" });
  usePaneStore.setState({
    teamKey: "acme:b",
    listing: { contentRoot: "/ctx/b", chain: ["b"], panes: [] },
    knownEntries: {},
    sourceRows: {
      "dashboard:team:acme:a/agents": {
        id: "dashboard:team:acme:a/agents",
        dashboardRef: ref,
        entry,
        contentRoot: "/ctx/a",
        sourceRevision: 1,
        listed: false,
      },
    },
  });
  useDashboardProfileStore.setState({
    org: "acme",
    profile: {
      id: "default",
      name: "Default",
      posture: "",
      slots: { dock: ref },
      cliLayout: "default",
      chrome: {},
    },
  });
});

describe("Agents list row", () => {
  it("shows a team-A choice as chosen, not missing, on sibling team B", async () => {
    listPanes.mockResolvedValue({ contentRoot: "/ctx/a", chain: ["a"], panes: [entry] });
    render(<GeneralSection query="agents list" />);
    const option = await screen.findByTestId(
      "settings-dock-dashboard-dashboard:team:acme:a/agents",
    );
    expect(listPanes).not.toHaveBeenCalled();
    expect(option).toHaveTextContent("Team agents");
    expect(option).not.toHaveTextContent("missing");
    expect(option).toHaveAttribute("aria-checked", "true");
  });

  it("says missing only when the choice's own team does not have it", async () => {
    usePaneStore.setState((s) => ({
      sourceRows: {
        "dashboard:team:acme:a/agents": {
          ...s.sourceRows["dashboard:team:acme:a/agents"],
          entry: null,
        },
      },
    }));
    render(<GeneralSection query="agents list" />);
    await waitFor(() =>
      expect(
        screen.getByTestId("settings-dock-dashboard-dashboard:team:acme:a/agents"),
      ).toHaveTextContent("agents · missing"),
    );
  });
});

it("offers the HTML list and the classic list, and the unset choice is the classic one while the HTML default is off", async () => {
  useDashboardProfileStore.setState({
    profile: { ...useDashboardProfileStore.getState().profile!, slots: {} },
  });
  render(<GeneralSection query="agents list" />);
  const html = screen.getByRole("radio", { name: "Agent list (HTML)" });
  const classic = screen.getByRole("radio", { name: "Built-in list (classic)" });
  expect(classic).toHaveAttribute("aria-checked", "true");
  expect(html).toHaveAttribute("aria-checked", "false");
  fireEvent.click(html);
  await waitFor(() =>
    expect(writeDashboardProfile).toHaveBeenLastCalledWith(
      "acme",
      expect.objectContaining({
        slots: { dock: { scope: "built-in", org: "acme", id: "agent-list" } },
      }),
      undefined,
    ),
  );
  await waitFor(() => expect(html).toHaveAttribute("aria-checked", "true"));
  fireEvent.click(classic);
  await waitFor(() =>
    expect(writeDashboardProfile).toHaveBeenLastCalledWith(
      "acme",
      expect.objectContaining({ slots: { dock: "native" } }),
      undefined,
    ),
  );
  await waitFor(() => expect(classic).toHaveAttribute("aria-checked", "true"));
});
