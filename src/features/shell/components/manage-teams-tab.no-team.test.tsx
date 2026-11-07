// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

// SW-FU-B7: the REAL Teams tab body, rendered in the no-team tab host — the
// onboarding path that creates the FIRST team must work with no team active.

vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));

import { WORKSPACES_VIRTUAL_PATH } from "@/features/org/lib/open-workspaces";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import type { Org } from "@/shared/types/domain";

import { openManageTeams } from "../manage-teams-virtual-tab";
import { NoTeamTabHost } from "./no-team-tab-host";

const createTeam = vi.fn(() => Promise.resolve());

function renderHost() {
  render(
    <NoTeamTabHost>
      <div data-testid="empty-state" />
    </NoTeamTabHost>,
  );
}

beforeEach(() => {
  createTeam.mockClear();
  useUiStore.setState({ noTeamTabs: [], noTeamActiveTab: null, manageTeamsIntent: null });
});

afterEach(() => {
  cleanup();
  useOrgStore.setState({ orgs: [], activeOrgSlug: null, activeTeamName: null });
});

describe("Teams tab in the no-team tab host", () => {
  it("creates the first team in a workspace that has none", async () => {
    useOrgStore.setState({
      orgs: [{ slug: "acme", display_name: "Acme", teams: [] } as unknown as Org],
      activeOrgSlug: "acme",
      activeTeamName: null,
      createTeam,
    });
    renderHost();
    act(() => openManageTeams("create"));

    expect(screen.getByTestId("manage-teams-tab")).toBeInTheDocument();
    const input = screen.getByLabelText("Team name");
    await waitFor(() => expect(input).toHaveFocus());
    fireEvent.change(input, { target: { value: "platform" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Create team" }));
    });
    expect(createTeam).toHaveBeenCalledWith("platform");
  });

  it("with no workspace open, 'Open workspaces' opens the Workspaces tab", () => {
    renderHost();
    act(() => openManageTeams());
    fireEvent.click(screen.getByTestId("manage-teams-open-workspaces"));
    expect(useUiStore.getState().noTeamActiveTab).toBe(WORKSPACES_VIRTUAL_PATH);
  });
});
