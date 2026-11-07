// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { createInvokeRouter } from "@/app/mock/mock-router";
import { MockEventBus } from "@/app/mock/mock-events";
import { buildTauriInternals } from "@/app/mock/tauri-internals";
import { createMockAgents } from "@/app/mock/handlers/agents";
import { resetRoutinesStoreForTests } from "@/features/routines/stores/routines-store";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { OwningScopeProvider, warmScopes } from "@/shared/lib/warm-scopes";

import { openAgentAccount } from "../agents-virtual-tab";
import { resetAgentsStoreForTests, useAgentsStore } from "../stores/agents-store";
import { AgentsTab } from "./agents-tab";

/**
 * One-shot requests to the Agents tab (open an agent's account picker, jump to an editor
 * section): a request is for one team's tab and one agent, only that team's shown tab acts on it,
 * and leaving it unanswered never leaves it waiting for a later, unrelated selection.
 */

vi.mock("@/shared/lib/open-management-tab", () => ({ openManagementTab: () => {} }));

const TRIAGE = "acme/backend/triage";
const SCRIBE = "acme/backend/scribe";
const A = "acme:backend";
const B = "acme:other";

beforeEach(() => {
  resetAgentsStoreForTests();
  resetRoutinesStoreForTests();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "backend" });
  useViewerStore.setState((s) => ({
    activeTab: { ...s.activeTab, [A]: "virtual://agents", [B]: "virtual://agents" },
    layoutMode: "viewer",
  }));
  useLocalProfileStore.setState({
    status: { active: { principal: "1", displayName: "Alice" } },
  } as unknown as Parameters<typeof useLocalProfileStore.setState>[0]);
  const events = new MockEventBus();
  const world = createMockAgents(events, { running: [`${TRIAGE}@3`] });
  const internals = buildTauriInternals(
    createInvokeRouter({ ...events.handlers(), ...world.handlers }),
  );
  events.bind(internals.runCallback);
  const w = window as unknown as Record<string, unknown>;
  w.__TAURI_INTERNALS__ = internals;
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
});

afterEach(() => {
  cleanup();
  resetAgentsStoreForTests();
  resetRoutinesStoreForTests();
});

/** Two retained teams' Agents tabs, `shown` the one on screen. */
async function renderRetained(shown: string) {
  act(() => warmScopes.activate(shown));
  useOrgStore.setState({ activeTeamName: shown.split(":")[1] });
  render(
    <TooltipProvider>
      {[A, B].map((team) => (
        <OwningScopeProvider key={team} scope={team}>
          <AgentsTab filePath="virtual://agents" teamContentPath="" hostTeamKey={team} />
        </OwningScopeProvider>
      ))}
    </TooltipProvider>,
  );
  await waitFor(() => expect(screen.getAllByTestId("agent-row").length).toBeGreaterThan(0));
}

async function showOverview(key: string) {
  act(() => useAgentsStore.getState().select(key));
  await waitFor(() => expect(screen.getAllByTestId("agent-overview").length).toBeGreaterThan(0));
}

describe("a request to open an agent's account picker", () => {
  it("opens the shown team's picker once; going back to the other team finds none open", async () => {
    await renderRetained(B);
    await showOverview(SCRIBE);
    act(() => openAgentAccount(SCRIBE));
    expect(await screen.findAllByRole("menu")).toHaveLength(1);
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
    // Back to the first team: its retained picker was never asked, so it isn't open.
    act(() => warmScopes.activate(A));
    act(() => useOrgStore.setState({ activeTeamName: "backend" }));
    await new Promise((r) => setTimeout(r, 50));
    expect(screen.queryByRole("menu")).toBeNull();
    expect(useAgentsStore.getState().request).toBeNull();
  });

  it("is dropped when the leave prompt it waited behind is cancelled", async () => {
    await renderRetained(A);
    await showOverview(TRIAGE);
    // An unsaved edit asks before leaving: the launch sheet's Fix for another agent waits on it.
    act(() => useAgentsStore.setState({ formDirty: true }));
    act(() => openAgentAccount(SCRIBE));
    expect(useAgentsStore.getState().pendingLeave).not.toBeNull();
    act(() => useAgentsStore.getState().cancelLeave());
    expect(useAgentsStore.getState().request).toBeNull();
    // Selecting that agent the ordinary way later opens nothing.
    act(() => useAgentsStore.setState({ formDirty: false }));
    act(() => useAgentsStore.getState().select(SCRIBE));
    await waitFor(() => expect(useAgentsStore.getState().selectedKey).toBe(SCRIBE));
    await new Promise((r) => setTimeout(r, 50));
    expect(screen.queryByRole("menu")).toBeNull();
  });
});

describe("a request to jump to an editor section", () => {
  it("moves the shown team's editor, not the retained hidden team's", async () => {
    await renderRetained(B);
    await showOverview(TRIAGE);
    act(() => useAgentsStore.getState().setView("edit"));
    await waitFor(() => expect(screen.getAllByTestId("agent-editor-nav")).toHaveLength(2));
    const [hiddenNav, shownNav] = screen.getAllByTestId("agent-editor-nav");
    // Which team's tab is the first editor in: the one for A, which is not shown.
    const current = (nav: HTMLElement) =>
      within(nav)
        .getAllByRole("button")
        .find((b) => b.getAttribute("aria-current") === "true")?.textContent;
    act(() => useAgentsStore.getState().openEditSection(TRIAGE, "permissions"));
    await waitFor(() => expect(current(shownNav)).toContain("Permissions"));
    expect(current(hiddenNav)).toContain("Basics");
  });
});
