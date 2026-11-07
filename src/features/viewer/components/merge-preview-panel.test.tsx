// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

// Stub the CM6 editor to a controlled node so the panel's read-only wiring +
// value plumbing is the subject under test (CM internals live in their own suite).
vi.mock("@/shared/components/code-editor", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/components/code-editor")>();
  return {
    ...actual,
    CodeEditor: ({
      value,
      readOnly,
      ariaLabel,
    }: {
      value: string;
      readOnly?: boolean;
      ariaLabel?: string;
    }) => (
      <div data-testid="mock-code-editor" data-readonly={String(readOnly)} data-aria={ariaLabel}>
        <textarea data-testid="editor-value" readOnly value={value} />
      </div>
    ),
  };
});

const listReposMock = vi.fn();
const previewMock = vi.fn();
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  listMergePreviewRepos: (...a: unknown[]) => listReposMock(...a),
  previewMergeMarkedFile: (...a: unknown[]) => previewMock(...a),
}));

import { MergePreviewPanel } from "./merge-preview-panel";

const CTX = {
  orgSlug: "acme",
  teamName: "default",
  codesenseHome: "/home/.codesense",
  userId: 42,
};

beforeEach(() => {
  listReposMock.mockReset();
  previewMock.mockReset();
});

describe("MergePreviewPanel (SW-CP-29)", () => {
  it("AC-1/AC-5: renders the merged result read-only for the first repo, labeled a snapshot", async () => {
    listReposMock.mockResolvedValue([{ owner: "acme", repo: "api", codeRepoPath: "/repos/api" }]);
    previewMock.mockResolvedValue({
      text: '{\n  "merged": true\n}',
      outcome: "replace",
      note: "A launch replaces the copy CodeSense wrote here (nobody edited it).",
    });

    render(<MergePreviewPanel ctx={CTX} logicalPath=".mcp.json" filename=".mcp.json" />);

    await waitFor(() =>
      expect((screen.getByTestId("editor-value") as HTMLTextAreaElement).value).toContain(
        '"merged": true',
      ),
    );
    // Read-only by construction (AC-5).
    expect(screen.getByTestId("mock-code-editor").getAttribute("data-readonly")).toBe("true");
    // Auto-selected the only repo and computed against it.
    expect(previewMock).toHaveBeenCalledWith(
      "/home/.codesense",
      42,
      "acme",
      "default",
      ".mcp.json",
      "/repos/api",
    );
    // Snapshot-in-time label present.
    expect(screen.getByText(/snapshot now/)).toBeInTheDocument();
    // SW-XS-8b: what Step 2 does with it there.
    expect(screen.getByTestId("merge-preview-outcome")).toHaveTextContent(
      "A launch replaces the copy CodeSense wrote here",
    );
  });

  it("AC-4: switching repos re-renders a different merged result", async () => {
    listReposMock.mockResolvedValue([
      { owner: "acme", repo: "api", codeRepoPath: "/repos/api" },
      { owner: "acme", repo: "web", codeRepoPath: "/repos/web" },
    ]);
    previewMock.mockImplementation((...a: unknown[]) =>
      Promise.resolve({
        text: a[5] === "/repos/web" ? "WEB-MERGE" : "API-MERGE",
        outcome: "write",
        note: "A launch writes this merged file here (it's absent).",
      }),
    );

    render(<MergePreviewPanel ctx={CTX} logicalPath=".mcp.json" filename=".mcp.json" />);

    await waitFor(() =>
      expect((screen.getByTestId("editor-value") as HTMLTextAreaElement).value).toBe("API-MERGE"),
    );

    await userEvent.selectOptions(screen.getByTestId("merge-preview-repo-select"), "/repos/web");

    await waitFor(() =>
      expect((screen.getByTestId("editor-value") as HTMLTextAreaElement).value).toBe("WEB-MERGE"),
    );
  });

  it("renders an explicit empty state when the team has no attached repos", async () => {
    listReposMock.mockResolvedValue([]);
    render(<MergePreviewPanel ctx={CTX} logicalPath=".mcp.json" filename=".mcp.json" />);
    await waitFor(() => expect(screen.getByTestId("merge-preview-no-repos")).toBeInTheDocument());
    expect(previewMock).not.toHaveBeenCalled();
  });

  it("surfaces a backend preview error without crashing", async () => {
    listReposMock.mockResolvedValue([{ owner: "acme", repo: "api", codeRepoPath: "/repos/api" }]);
    previewMock.mockRejectedValue(new Error("read repo target file: boom"));

    render(<MergePreviewPanel ctx={CTX} logicalPath=".mcp.json" filename=".mcp.json" />);

    await waitFor(() => expect(screen.getByTestId("merge-preview-error")).toBeInTheDocument());
    expect(screen.getByText(/read repo target file: boom/)).toBeInTheDocument();
  });
});
