// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";

import { workspaceDisplayLabel } from "@/shared/lib/worktree";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

import { setTrayReorderIntent } from "../lib/tray-reorder-drag";
import { WorkspaceDragGhost } from "./workspace-drag-ghost";

const story = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });

describe("WorkspaceDragGhost", () => {
  const TEAM = "acme:default";
  afterEach(() => {
    cleanup();
    setTrayReorderIntent(null);
    useViewerStore.setState({ workspaces: {}, softClosedWorkspaces: {} });
  });

  it("names a workspace of the team, and a minimized one", () => {
    const tile = story("o-1");
    const minimized = story("o-2");
    useViewerStore.setState({
      workspaces: { [TEAM]: [tile] },
      softClosedWorkspaces: { [TEAM]: { "o-2": { workspace: minimized } } },
    });
    const view = render(<WorkspaceDragGhost teamKey={TEAM} workspaceId="o-1" />);
    expect(screen.getByTestId("workspace-drag-ghost")).toHaveTextContent(
      workspaceDisplayLabel(tile),
    );
    view.rerender(<WorkspaceDragGhost teamKey={TEAM} workspaceId="o-2" />);
    expect(screen.getByTestId("workspace-drag-ghost")).toHaveTextContent(
      workspaceDisplayLabel(minimized),
    );
  });

  it("hides while the pointer is in the tray's band, where the drag reorders", () => {
    useViewerStore.setState({ workspaces: { [TEAM]: [story("o-1")] } });
    render(<WorkspaceDragGhost teamKey={TEAM} workspaceId="o-1" />);
    expect(screen.getByTestId("workspace-drag-ghost")).toBeInTheDocument();
    act(() => setTrayReorderIntent({ teamKey: TEAM, workspaceId: "o-1", place: null }));
    expect(screen.queryByTestId("workspace-drag-ghost")).toBeNull();
    act(() => setTrayReorderIntent(null));
    expect(screen.getByTestId("workspace-drag-ghost")).toBeInTheDocument();
  });
});
