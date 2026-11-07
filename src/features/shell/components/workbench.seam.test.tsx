// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { installMockBackend } from "@/app/mock/install";
import { MOCK_ORG_SLUG } from "@/app/mock/fixtures";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

/** Which team's pane should throw on its next render (a crash in one team's portaled pane). */
const crash = vi.hoisted(() => ({ team: null as string | null }));

// Each team's "Launch agent" context, tagged with its team so a pane can say whose it reads.
vi.mock("@/features/story-launch/use-story-agent-launch", async (importOriginal) => {
  const real =
    await importOriginal<typeof import("@/features/story-launch/use-story-agent-launch")>();
  return {
    ...real,
    StoryAgentLaunchProvider: ({
      teamKey,
      children,
    }: {
      teamKey: string;
      children: React.ReactNode;
    }) => (
      <real.StoryAgentLaunchActionsProvider
        value={{ agents: [], launch: () => {}, shortcutLabel: teamKey }}
      >
        {children}
      </real.StoryAgentLaunchActionsProvider>
    ),
  };
});

// The right pane, as a probe: a counter in local React state, and the team whose launch context
// it reads. It throws while its team is the one marked to crash.
vi.mock("@/features/viewer/components/right-pane-layout", async () => {
  const { useStoryAgentLaunchActions } =
    await import("@/features/story-launch/use-story-agent-launch");
  return {
    RightPaneLayout: () => {
      const team = useStoryAgentLaunchActions()?.shortcutLabel ?? "none";
      const [clicks, setClicks] = useState(0);
      if (crash.team === team) throw new Error(`pane of ${team} crashed`);
      return (
        <button type="button" data-testid={`pane-${team}`} onClick={() => setClicks((n) => n + 1)}>
          {clicks}
        </button>
      );
    },
  };
});

import { Workbench } from "./workbench";

const A = `${MOCK_ORG_SLUG}:default`;
const B = `${MOCK_ORG_SLUG}:other`;

async function mount() {
  const view = render(
    <TooltipProvider>
      <Workbench />
    </TooltipProvider>,
  );
  await act(async () => {});
  return view;
}

const switchTo = (teamName: string) =>
  act(() => useOrgStore.setState({ activeTeamName: teamName }));
const hostOf = (teamKey: string) =>
  document.querySelector<HTMLElement>(`[data-right-pane-host="${teamKey}"]`)!;

describe("Workbench: one right column, one host per team", () => {
  beforeEach(() => {
    crash.team = null;
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    installMockBackend();
    useOrgStore.setState({ activeOrgSlug: MOCK_ORG_SLUG, activeTeamName: "default" });
    useViewerStore.setState({
      openedTeams: [A, B],
      rightPaneOpen: true,
      rightPaneRatio: 0.3,
      layoutMode: "viewer",
      terminalVisible: false,
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    document.body.innerHTML = "";
    useViewerStore.setState({ openedTeams: [], rightPaneOpen: false, layoutMode: null });
    useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  it("draws each team's right pane in its own host in the right column, the active team's shown", async () => {
    await mount();
    await waitFor(() => expect(screen.getByTestId(`pane-${A}`)).toBeInTheDocument());
    const column = screen.getByTestId("right-pane-column");
    expect(hostOf(A).parentElement).toBe(column);
    expect(hostOf(A).contains(screen.getByTestId(`pane-${A}`))).toBe(true);
    expect(hostOf(A).style.display).toBe("");
    // The pane sits outside the team view's own subtree: the shell owns the right column.
    expect(screen.getByTestId(`pane-${A}`).closest("[data-viewer-content]")).toBeNull();
  });

  it("the right pane keeps its state across a switch away and back, and reads its own team's launch context", async () => {
    await mount();
    const paneA = await screen.findByTestId(`pane-${A}`);
    fireEvent.click(paneA);
    fireEvent.click(paneA);
    expect(paneA).toHaveTextContent("2");

    switchTo("other");
    await waitFor(() => expect(screen.getByTestId(`pane-${B}`)).toBeInTheDocument());
    expect(hostOf(B).style.display).toBe("");
    expect(hostOf(A).style.display).toBe("none");

    switchTo("default");
    await waitFor(() => expect(hostOf(A).style.display).toBe(""));
    // The same element, with its local state: the pane never remounted.
    expect(screen.getByTestId(`pane-${A}`)).toBe(paneA);
    expect(paneA).toHaveTextContent("2");
  });

  it("a crash in one team's portaled pane degrades only that team", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    await mount();
    await screen.findByTestId(`pane-${A}`);
    switchTo("other");
    await screen.findByTestId(`pane-${B}`);

    crash.team = B;
    act(() => useViewerStore.setState({ rightPaneRatio: 0.31 }));
    // B's re-render throws: B shows its crash fallback; A's view and pane stay alive.
    switchTo("default");
    act(() => useViewerStore.setState({ rightPaneRatio: 0.32 }));
    switchTo("other");
    await waitFor(() =>
      expect(document.querySelector(`[data-crashed-team="${B}"]`)).not.toBeNull(),
    );
    expect(document.querySelector(`[data-crashed-team="${A}"]`)).toBeNull();
    expect(screen.getByTestId(`pane-${A}`)).toBeInTheDocument();
    expect(screen.queryByTestId(`pane-${B}`)).toBeNull();
    crash.team = null;
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(screen.getByTestId(`pane-${B}`)).toBeInTheDocument());
    expect(document.querySelector(`[data-crashed-team="${B}"]`)).toBeNull();
    expect(screen.getByTestId(`pane-${A}`)).toBeInTheDocument();
  });

  it("a right-pane crash keeps its Retry visible in full-screen CLI", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    await mount();
    await screen.findByTestId(`pane-${A}`);
    act(() => useViewerStore.getState().setLayoutMode("console"));
    crash.team = A;
    act(() => useViewerStore.setState({ rightPaneRatio: 0.31 }));
    const retry = await screen.findByRole("button", { name: "Retry" });
    expect(retry.closest("[data-testid='workbench-viewer-region']")).toBeNull();
    expect(retry).toBeVisible();
    crash.team = null;
    fireEvent.click(retry);
    await screen.findByTestId(`pane-${A}`);
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    expect(useViewerStore.getState().layoutMode).toBe("console");
  });

  it("the right-pane divider drags the shell's right column", async () => {
    await mount();
    await screen.findByTestId(`pane-${A}`);
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      height: 600,
      width: 1000,
      top: 0,
      left: 0,
      right: 1000,
      bottom: 600,
      x: 0,
      y: 0,
      toJSON: () => "",
    });
    Element.prototype.setPointerCapture ??= () => {};
    Element.prototype.releasePointerCapture ??= () => {};
    const divider = screen.getByTestId("scratch-note-width-divider");
    fireEvent.pointerDown(divider, { pointerId: 1, clientX: 700 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientX: 600 });
    fireEvent.pointerUp(divider, { pointerId: 1, clientX: 600 });
    expect(useViewerStore.getState().rightPaneRatio).toBeCloseTo(0.4, 5);
  });

  it("closing the right pane hides the column, and the divider stays as the open target", async () => {
    await mount();
    await screen.findByTestId(`pane-${A}`);
    act(() => useViewerStore.setState({ rightPaneOpen: false }));
    expect(screen.getByTestId("right-pane-column").style.display).toBe("none");
    expect(screen.getByTestId("scratch-note-width-divider")).toBeInTheDocument();
    // Closing hides the pane; it does not unmount it.
    expect(screen.getByTestId(`pane-${A}`)).toBeInTheDocument();
  });
});
