// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

const getFileRedlineMock = vi.fn();
vi.mock("../lib/file-redline-source", async (orig) => {
  const actual = (await orig()) as object;
  return { ...actual, getFileRedline: (...a: unknown[]) => getFileRedlineMock(...a) };
});

// Stub the heavy viewers — assert which side/source they receive.
vi.mock("@/features/dashboard/components/excalidraw-viewer", () => ({
  ExcalidrawViewer: ({
    sceneText,
    readOnly,
  }: {
    sceneText?: string | null;
    readOnly?: boolean;
  }) => (
    <div
      data-testid="excalidraw"
      data-scene={sceneText ?? "DISK"}
      data-readonly={String(!!readOnly)}
    />
  ),
}));
vi.mock("@/features/dashboard/components/image-viewer", () => ({
  ImageViewer: ({ baselineSource }: { baselineSource?: unknown }) => (
    <div data-testid="image" data-baseline={baselineSource ? "yes" : "no"} />
  ),
}));
vi.mock("@/features/dashboard/components/html-viewer", () => ({
  HtmlViewer: ({ baselineSource }: { baselineSource?: unknown }) => (
    <div data-testid="html" data-baseline={baselineSource ? "yes" : "no"} />
  ),
}));

import { BeforeAfterDiffToggle } from "./before-after-diff-toggle";

const baseProps = {
  orgRoot: "/ctx",
  teamContentPath: "/ctx/_team/default/content",
  navRepoPath: "/ctx",
};

function redline(status: "added" | "modified" | "deleted", before = "B", after = "A") {
  return { status, binary: false, before, after };
}

describe("BeforeAfterDiffToggle (SW-GS-3)", () => {
  beforeEach(() => getFileRedlineMock.mockReset());

  it("a modified image shows a Before/After toggle, defaulting to After (working tree)", async () => {
    getFileRedlineMock.mockResolvedValue(redline("modified"));
    render(<BeforeAfterDiffToggle {...baseProps} absPath="/ctx/a.png" />);

    const img = await screen.findByTestId("image");
    expect(img).toHaveAttribute("data-baseline", "no"); // After = working tree
    expect(screen.getByRole("button", { name: "before" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "before" }));
    expect(screen.getByTestId("image")).toHaveAttribute("data-baseline", "yes");
  });

  it("a diff that can't be read says so instead of loading forever", async () => {
    getFileRedlineMock.mockImplementation(() =>
      Promise.reject(new Error("not a path inside the repo")),
    );
    render(<BeforeAfterDiffToggle {...baseProps} absPath="/ctx/a.png" />);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Couldn't read the changes: not a path inside the repo",
    );
    expect(screen.queryByText("Loading…")).not.toBeInTheDocument();
    getFileRedlineMock.mockReset();
  });

  it("an added file has no toggle and shows only the current version", async () => {
    getFileRedlineMock.mockResolvedValue(redline("added", "", "A"));
    render(<BeforeAfterDiffToggle {...baseProps} absPath="/ctx/a.png" />);

    expect(await screen.findByTestId("image")).toHaveAttribute("data-baseline", "no");
    expect(screen.queryByRole("button", { name: "before" })).not.toBeInTheDocument();
  });

  it("a deleted file has no toggle and shows only the baseline", async () => {
    getFileRedlineMock.mockResolvedValue(redline("deleted", "B", ""));
    render(<BeforeAfterDiffToggle {...baseProps} absPath="/ctx/a.png" />);

    expect(await screen.findByTestId("image")).toHaveAttribute("data-baseline", "yes");
    expect(screen.queryByRole("button", { name: "after" })).not.toBeInTheDocument();
  });

  it("excalidraw 'before' renders the baseline scene JSON in-band", async () => {
    getFileRedlineMock.mockResolvedValue(redline("modified", '{"baseline":1}', '{"current":1}'));
    render(<BeforeAfterDiffToggle {...baseProps} absPath="/ctx/a.excalidraw" />);

    // Default After → reads from disk, but read-only (no editing in the diff pane).
    const after = await screen.findByTestId("excalidraw");
    expect(after).toHaveAttribute("data-scene", "DISK");
    expect(after).toHaveAttribute("data-readonly", "true");
    fireEvent.click(screen.getByRole("button", { name: "before" }));
    const before = screen.getByTestId("excalidraw");
    expect(before).toHaveAttribute("data-scene", '{"baseline":1}');
    expect(before).toHaveAttribute("data-readonly", "true");
  });
});
