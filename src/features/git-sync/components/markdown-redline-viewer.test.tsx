// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import type { Root as HastRoot } from "hast";

const getFileRedlineMock = vi.fn();
vi.mock("../lib/file-redline-source", async (orig) => {
  const actual = (await orig()) as object;
  return { ...actual, getFileRedline: (...a: unknown[]) => getFileRedlineMock(...a) };
});

const parseToRedlineMock = vi.fn();
vi.mock("@/shared/lib/markdown-worker/client", () => ({
  parseToRedline: (before: string, after: string) => parseToRedlineMock(before, after),
}));

vi.mock("./changed-binary-affordance", () => ({
  ChangedBinaryAffordance: ({ status }: { status: string }) => (
    <div data-testid="cba">{status}</div>
  ),
}));

import { MarkdownRedlineViewer } from "./markdown-redline-viewer";

function pHast(text: string): HastRoot {
  return {
    type: "root",
    children: [
      { type: "element", tagName: "p", properties: {}, children: [{ type: "text", value: text }] },
    ],
  };
}

const baseProps = {
  orgRoot: "/ctx",
  absPath: "/ctx/a.md",
  teamKey: "acme:default",
  navRepoPath: "/ctx",
};

describe("MarkdownRedlineViewer (SW-GS-3)", () => {
  beforeEach(() => {
    getFileRedlineMock.mockReset();
    parseToRedlineMock.mockReset();
  });

  it("renders the worker redline output for a changed markdown file", async () => {
    getFileRedlineMock.mockResolvedValue({
      status: "modified",
      binary: false,
      before: "old",
      after: "new",
    });
    parseToRedlineMock.mockReturnValue({
      promise: Promise.resolve({ hast: pHast("redline-ok"), perf: {}, receiveAt: 0 }),
      cancel: vi.fn(),
    });

    render(<MarkdownRedlineViewer {...baseProps} />);

    expect(await screen.findByText("redline-ok")).toBeInTheDocument();
    // SW-GS-19/13: trailing prNumber + commitSha are undefined for working-tree.
    expect(getFileRedlineMock).toHaveBeenCalledWith(
      "/ctx",
      "a.md",
      undefined,
      undefined,
      undefined,
    );
    expect(parseToRedlineMock).toHaveBeenCalledWith("old", "new");
  });

  it("passes the pre-rename old path to the fetch so a moved doc diffs by old path (SW-GS-14)", async () => {
    getFileRedlineMock.mockResolvedValue({
      status: "modified",
      binary: false,
      before: "old",
      after: "new",
    });
    parseToRedlineMock.mockReturnValue({
      promise: Promise.resolve({ hast: pHast("redline-ok"), perf: {}, receiveAt: 0 }),
      cancel: vi.fn(),
    });

    render(<MarkdownRedlineViewer {...baseProps} absPath="/ctx/new.md" oldPath="old.md" />);

    expect(await screen.findByText("redline-ok")).toBeInTheDocument();
    // "before" is read from the old baseline path, "after" from the new path.
    expect(getFileRedlineMock).toHaveBeenCalledWith(
      "/ctx",
      "new.md",
      "old.md",
      undefined,
      undefined,
    );
  });

  it("shows the binary affordance when the backend flags binary", async () => {
    getFileRedlineMock.mockResolvedValue({
      status: "modified",
      binary: true,
      before: "",
      after: "",
    });

    render(<MarkdownRedlineViewer {...baseProps} absPath="/ctx/weird.md" />);

    expect(await screen.findByTestId("cba")).toHaveTextContent("modified");
    expect(parseToRedlineMock).not.toHaveBeenCalled();
  });
});
