// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@/test-utils";

import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import type { PublishFile } from "@/features/git-sync/lib/types";

// SLW-DV-1 AC-6 — one test that renders the worktree overlays (source diff,
// editor) and asserts each mounts through the shared
// `OverlayShell`: same popover token set, no `--border` vs `var(--border)` drift.
// The heavy bodies are stubbed so this stays a pure-chrome assertion.

const changesetMock = vi.fn();
const redlineMock = vi.fn();
vi.mock("../lib/use-source-diff", () => ({
  useSourceDiffBase: () => ({ baseRef: undefined, defaultStart: "main" }),
  useSourceChangeset: (...a: unknown[]) => changesetMock(...a),
  useSourceFileRedline: (...a: unknown[]) => redlineMock(...a),
  useSourceCommitChangeset: () => ({ files: null, loading: false, error: null }),
  useSourceCommitRedline: () => ({ redline: null, error: null }),
}));
vi.mock("../lib/use-worktree-watch", () => ({ useWorktreeWatch: vi.fn() }));

// The diff's code-diff renderer and the editor's CM6 editors are exercised in
// their own suites — stub them to markers, keeping the path helpers real.
vi.mock("@/shared/components/code-editor", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/components/code-editor")>()),
  CodeDiffReview: () => <div data-testid="mock-code-diff" />,
  CodeEditor: () => <div data-testid="mock-code-editor" />,
  CodeMergeView: () => <div data-testid="mock-code-merge" />,
}));

// The editor's file tree is its own subject elsewhere.
vi.mock("./worktree-file-tree", () => ({
  WorktreeFileTree: () => <div data-testid="mock-tree" />,
}));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  writeWorktreeFile: vi.fn(),
  routeFileToContext: vi.fn(),
  unrouteFileFromContext: vi.fn(),
  detectTools: () =>
    Promise.resolve({ terminals: [], editors: [], default_terminal: "", default_editor: "" }),
}));

import { SourceDiffOverlay } from "./source-diff-overlay";
import { SourceEditorInset } from "./source-editor-inset";

// The single token set the shell owns — every overlay's chrome must read it, and
// none may carry a raw hex color.
const SHELL_TOKENS = [
  "border-[var(--border)]",
  "bg-[var(--popover)]",
  "text-[var(--popover-foreground)]",
];

function expectShellChrome(testId: string) {
  const shell = screen.getByTestId(testId);
  for (const token of SHELL_TOKENS) expect(shell.className).toContain(token);
  expect(shell.className).not.toMatch(/#[0-9a-fA-F]{3,6}/);
  return shell;
}

const storyWs: Workspace = {
  id: "w1",
  kind: "story",
  cwd: "/wt/story",
  status: "ready",
  storyId: "SLW-DV-1",
  storyFilePath: "/repo/story.md",
  sourceRepoPath: "/repo",
};

beforeEach(() => {
  vi.clearAllMocks();
  changesetMock.mockReturnValue({
    files: [{ path: "src/mod.ts", status: "modified" }] as PublishFile[],
    loading: false,
    error: null,
    reload: vi.fn(),
  });
  redlineMock.mockReturnValue({
    redline: { status: "modified", binary: false, before: "old", after: "new" },
    error: null,
  });
  useViewerStore.setState({
    sourceDiffShownBySession: { "sid-ws": true },
    sourceDiffSelectedFile: { w1: "src/mod.ts" },
    sourceDiffHistoryMode: { w1: false },
    sourceDiffViewOptions: {},
    worktreeEditorOpenBySession: {},
    sourceDiffSelectedSha: {},
  });
});

describe("SLW-DV-1 — the overlays share one shell (AC-2/AC-3/AC-4/AC-6)", () => {
  it("the source diff expanded reader mounts through OverlayShell", () => {
    render(
      <SourceDiffOverlay
        workspace={storyWs}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expectShellChrome("source-diff-overlay-expanded-w1");
  });

  it("the source editor inset mounts through OverlayShell", () => {
    render(<SourceEditorInset workspace={storyWs} seedRelPath={null} onClose={vi.fn()} />);
    expectShellChrome("source-editor-inset-w1");
  });
});
