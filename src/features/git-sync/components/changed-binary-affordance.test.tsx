// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

import { useViewerStore } from "@/shared/stores/viewer-store";

import { ChangedBinaryAffordance } from "./changed-binary-affordance";

describe("ChangedBinaryAffordance (SW-GS-3)", () => {
  let setTabDiff: ReturnType<typeof vi.fn>;
  let openFile: ReturnType<typeof vi.fn>;
  beforeEach(() => {
    setTabDiff = vi.fn();
    openFile = vi.fn();
    useViewerStore.setState({
      setTabDiff: setTabDiff as unknown as ReturnType<typeof useViewerStore.getState>["setTabDiff"],
      openFile: openFile as unknown as (t: string, r: string, f: string) => void,
    });
  });

  it("'open current' opens the working-tree file on its File side", () => {
    render(
      <ChangedBinaryAffordance
        absPath="/ctx/diagram.excalidraw"
        status="modified"
        teamKey="acme:default"
        navRepoPath="/ctx"
      />,
    );
    expect(screen.getByText("diagram.excalidraw")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /open current version/i }));
    expect(openFile).toHaveBeenCalledWith("acme:default", "/ctx", "/ctx/diagram.excalidraw");
    expect(setTabDiff).toHaveBeenCalledWith("acme:default", "/ctx/diagram.excalidraw", false);
  });

  it("a deleted file offers no 'open current' (the working-tree copy is gone)", () => {
    render(
      <ChangedBinaryAffordance
        absPath="/ctx/old.png"
        status="deleted"
        teamKey="acme:default"
        navRepoPath="/ctx"
      />,
    );
    expect(screen.queryByRole("button", { name: /open current/i })).not.toBeInTheDocument();
  });
});
