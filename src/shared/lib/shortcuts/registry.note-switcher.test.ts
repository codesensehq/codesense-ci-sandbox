// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

// The action's run() resolves the active team via getActiveWorkspaceContext;
// mock it so we can drive both the happy path and the NO_TEAM guard.
const getActiveWorkspaceContextMock = vi.fn();
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  getActiveWorkspaceContext: () => getActiveWorkspaceContextMock(),
  // The CLI shows the active team in these tests.
}));

import { NO_TEAM_SENTINEL } from "@/shared/lib/team-key";
import { useUiStore } from "@/shared/stores/ui-store";
import { SHORTCUT_ACTIONS } from "./registry";

function runNoteSwitcher() {
  const action = SHORTCUT_ACTIONS.find((a) => a.id === "nav.open-note-switcher");
  if (!action) throw new Error("nav.open-note-switcher not registered");
  action.run();
}

describe("nav.open-note-switcher run() (SW-SN-3)", () => {
  beforeEach(() => {
    useUiStore.setState({ noteSwitcherOpen: false });
    vi.clearAllMocks();
  });

  it("flips noteSwitcherOpen true when a team is active (Cmd+O dispatch)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: "acme:default" });
    runNoteSwitcher();
    expect(useUiStore.getState().noteSwitcherOpen).toBe(true);
  });

  it("no-ops when no team is active (NO_TEAM_SENTINEL guard)", () => {
    getActiveWorkspaceContextMock.mockReturnValue({ teamKey: NO_TEAM_SENTINEL });
    runNoteSwitcher();
    expect(useUiStore.getState().noteSwitcherOpen).toBe(false);
  });
});
