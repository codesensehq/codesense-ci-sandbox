// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

const isMarkedMock = vi.fn<(p: string) => boolean>();
vi.mock("@/features/dashboard/lib/use-merge-marks", () => ({
  useMergeMarks: () => ({ isMarked: isMarkedMock, toggle: vi.fn() }),
}));

let ctxValue: unknown = {
  orgSlug: "acme",
  teamName: "default",
  codesenseHome: "/home/.codesense",
  userId: 42,
};
vi.mock("@/shared/hooks/use-resolver-context", () => ({
  useReadyResolverContext: () => ctxValue,
}));

vi.mock("@/shared/stores/use-active-org", () => ({
  useActiveOrg: () => ({ org_root: "/home/.codesense/users/42/orgs/acme" }),
}));

// Stub the merged panel — its own behavior is covered in merge-preview-panel.test.
vi.mock("./merge-preview-panel", () => ({
  MergePreviewPanel: ({ logicalPath }: { logicalPath: string }) => (
    <div data-testid="merged-panel">merged:{logicalPath}</div>
  ),
}));

import { MergeMarkedViewer } from "./merge-marked-viewer";

const TEAM = "/home/.codesense/users/42/orgs/acme/default/content";

beforeEach(() => {
  isMarkedMock.mockReset();
  ctxValue = {
    orgSlug: "acme",
    teamName: "default",
    codesenseHome: "/home/.codesense",
    userId: 42,
  };
});

function Authored(): React.ReactElement {
  return <div data-testid="authored">authored</div>;
}

describe("MergeMarkedViewer (SW-CP-29)", () => {
  it("passes through with no chrome for a non-marked file", () => {
    isMarkedMock.mockReturnValue(false);
    render(
      <MergeMarkedViewer absPath={`${TEAM}/.mcp.json`} teamContentPath={TEAM} enabled>
        <Authored />
      </MergeMarkedViewer>,
    );
    expect(screen.getByTestId("authored")).toBeInTheDocument();
    expect(screen.queryByTestId("merge-marked-view-toggle")).not.toBeInTheDocument();
  });

  it("passes through for a marked but non-structured file (canMergeMark gate)", () => {
    isMarkedMock.mockReturnValue(true);
    render(
      <MergeMarkedViewer absPath={`${TEAM}/notes.txt`} teamContentPath={TEAM} enabled>
        <Authored />
      </MergeMarkedViewer>,
    );
    expect(screen.queryByTestId("merge-marked-view-toggle")).not.toBeInTheDocument();
  });

  it("passes through when disabled (sync redline surface)", () => {
    isMarkedMock.mockReturnValue(true);
    render(
      <MergeMarkedViewer absPath={`${TEAM}/.mcp.json`} teamContentPath={TEAM} enabled={false}>
        <Authored />
      </MergeMarkedViewer>,
    );
    expect(screen.queryByTestId("merge-marked-view-toggle")).not.toBeInTheDocument();
  });

  it("shows the toggle for a marked config file; swaps authored ↔ merged, deriving the content-relative path", async () => {
    isMarkedMock.mockReturnValue(true);
    render(
      <MergeMarkedViewer absPath={`${TEAM}/.mcp.json`} teamContentPath={TEAM} enabled>
        <Authored />
      </MergeMarkedViewer>,
    );

    // The marks lookup used the content-relative logical path, not the abs path.
    expect(isMarkedMock).toHaveBeenCalledWith(".mcp.json");

    // Default = authored.
    expect(screen.getByTestId("authored")).toBeInTheDocument();
    expect(screen.queryByTestId("merged-panel")).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Merged preview" }));
    expect(screen.getByTestId("merged-panel")).toHaveTextContent("merged:.mcp.json");
    expect(screen.queryByTestId("authored")).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Authored" }));
    expect(screen.getByTestId("authored")).toBeInTheDocument();
  });

  it("passes through when the file is not under the team content root", () => {
    isMarkedMock.mockReturnValue(true);
    render(
      <MergeMarkedViewer absPath="/somewhere/else/.mcp.json" teamContentPath={TEAM} enabled>
        <Authored />
      </MergeMarkedViewer>,
    );
    // The abs path is never used as a marks key (would be a false gate).
    expect(isMarkedMock).not.toHaveBeenCalledWith("/somewhere/else/.mcp.json");
    expect(screen.getByTestId("authored")).toBeInTheDocument();
    expect(screen.queryByTestId("merge-marked-view-toggle")).not.toBeInTheDocument();
  });

  it("passes through when there is no resolved team context", () => {
    isMarkedMock.mockReturnValue(true);
    ctxValue = null;
    render(
      <MergeMarkedViewer absPath={`${TEAM}/.mcp.json`} teamContentPath={TEAM} enabled>
        <Authored />
      </MergeMarkedViewer>,
    );
    expect(screen.getByTestId("authored")).toBeInTheDocument();
    expect(screen.queryByTestId("merge-marked-view-toggle")).not.toBeInTheDocument();
  });
});
