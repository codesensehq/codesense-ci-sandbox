// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, act, within } from "@/test-utils";

import type { Workspace } from "@/shared/stores/viewer-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import type { WorktreeFileSelection } from "./worktree-file-tree";

// Drive selection through a stub tree so the inset's read/gate/edit logic is the
// subject under test (the real tree has its own suite).
vi.mock("./worktree-file-tree", () => ({
  WorktreeFileTree: ({
    onSelect,
    selectedPath,
  }: {
    onSelect: (f: WorktreeFileSelection) => void;
    selectedPath: string | null;
  }) => (
    <div data-testid="mock-tree" data-selected={selectedPath ?? ""}>
      <button
        type="button"
        data-testid="pick-large"
        onClick={() => onSelect({ path: "/wt/huge.log", size: 5_000_000 })}
      >
        large
      </button>
      <button
        type="button"
        data-testid="pick-text"
        onClick={() => onSelect({ path: "/wt/a.ts", size: 10 })}
      >
        text
      </button>
      <button
        type="button"
        data-testid="pick-other"
        onClick={() => onSelect({ path: "/wt/b.ts", size: 10 })}
      >
        other
      </button>
      <button
        type="button"
        data-testid="pick-binary"
        onClick={() => onSelect({ path: "/wt/img.png", size: 10 })}
      >
        binary
      </button>
    </div>
  ),
}));

// The stub editor's imperative handle (go to line, find) — asserted by the
// open-at-line and keyboard tests.
const editorHandle = vi.hoisted(() => ({
  revealLine: vi.fn<(line: number, column?: number) => void>(),
  openSearch: vi.fn<() => void>(),
  focus: vi.fn<() => void>(),
}));

// Stub the CM6 editor to a controlled textarea + a Cmd-S trigger. The real
// editable wiring (value/onChange/onSave) is asserted here; CM internals + the
// Mod-s keymap are covered in the code-editor suites.
vi.mock("@/shared/components/code-editor", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/components/code-editor")>();
  const React = await import("react");
  return {
    ...actual,
    CodeEditor: React.forwardRef(function MockCodeEditor(
      {
        value,
        path,
        readOnly,
        onChange,
        onSave,
      }: {
        value: string;
        path?: string;
        readOnly?: boolean;
        onChange?: (v: string) => void;
        onSave?: (v: string) => Promise<void>;
      },
      ref: React.ForwardedRef<unknown>,
    ) {
      React.useImperativeHandle(ref, () => ({
        revealLine: editorHandle.revealLine,
        openSearch: editorHandle.openSearch,
        focus: editorHandle.focus,
      }));
      return (
        <div
          className="cm-editor"
          data-testid="mock-code-editor"
          data-path={path}
          data-readonly={String(readOnly)}
        >
          <textarea
            data-testid="editor-textarea"
            value={value}
            onChange={(e) => onChange?.(e.target.value)}
          />
          {/* Simulates the editor's built-in ⌘S keymap calling onSave(currentDoc). */}
          <button type="button" data-testid="editor-cmd-s" onClick={() => void onSave?.(value)}>
            cmd-s
          </button>
        </div>
      );
    }),
    // SLW-WDE-4: stub the editable merge view. `data-disk` exposes the (read-only)
    // disk side for the AC-5 live-refresh assertion; the right side is an
    // uncontrolled textarea (so a re-render for a fresh diskValue does NOT reset
    // typed edits — that's how we prove "no remount"); ⌘S and getLocalValue() (the
    // "Keep mine" ref path) both read its current value. CM6 internals + the
    // skeleton/Retry loader live in the CodeMergeView suites.
    CodeMergeView: React.forwardRef(function MockMergeView(
      {
        diskValue,
        localValue,
        onSave,
      }: {
        diskValue: string;
        localValue: string;
        onSave?: (v: string) => Promise<void>;
      },
      ref: React.ForwardedRef<{ getLocalValue: () => string }>,
    ) {
      const taRef = React.useRef<HTMLTextAreaElement>(null);
      React.useImperativeHandle(ref, () => ({
        getLocalValue: () => taRef.current?.value ?? localValue,
      }));
      return (
        <div data-testid="mock-merge-view" data-disk={diskValue}>
          <textarea data-testid="merge-local" defaultValue={localValue} ref={taRef} />
          <button
            type="button"
            data-testid="merge-cmd-s"
            onClick={() => void onSave?.(taRef.current?.value ?? localValue)}
          >
            merge-save
          </button>
        </div>
      );
    }),
  };
});

const readFileMock = vi.fn<(p: string) => Promise<string>>();
const writeWorktreeFileMock = vi.fn<(p: string, c: string) => Promise<void>>();
/** The optimistic-lock token each local write carried (kept apart so the
 *  two-argument assertions above stay exact). */
const writeHashes: (string | undefined)[] = [];
const lazyAttachWorktreeWatchMock = vi.fn<(id: string, cwd: string) => Promise<number>>();
const detachWorktreeWatchMock = vi.fn<(id: string, token: number) => Promise<void>>();
const worktreeListFilesMock = vi.fn();
const worktreeFileHeadMock = vi.fn();
const remote = {
  list: vi.fn(),
  read: vi.fn(),
  write: vi.fn(),
  inspect: vi.fn(),
  head: vi.fn(),
  files: vi.fn(),
};
// SW-VE-12 (B4) → SW-VE-14: the editor reads `useSessionCwd` to retarget on a
// terminal `cd` and `useSessionCwdRevision` bumps per-session on OSC 7.
// Default to null + 0 so existing suites that don't exercise the override
// behave exactly as before (`liveCwd=null` → fall back to `ws.cwd`).
const sessionCwdMock = vi.hoisted(() =>
  vi.fn<(sid: string | undefined, rev: number) => string | null>(() => null),
);
vi.mock("@/features/viewer/hooks/use-session-cwd", () => ({
  useSessionCwd: (sid: string | undefined, rev: number) => sessionCwdMock(sid, rev),
}));
const sessionCwdRevisionMock = vi.hoisted(() => vi.fn<() => number>(() => 0));
vi.mock("@/features/viewer/hooks/use-session-cwd-revision", () => ({
  useSessionCwdRevision: () => sessionCwdRevisionMock(),
}));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  readFile: (p: string) => readFileMock(p),
  writeWorktreeFile: (p: string, c: string, h?: string) => {
    writeHashes.push(h);
    return writeWorktreeFileMock(p, c);
  },
  // SLW-WDE-3: the worktree-watch lifecycle commands — off real IPC + asserted.
  lazyAttachWorktreeWatch: (id: string, cwd: string) => lazyAttachWorktreeWatchMock(id, cwd),
  detachWorktreeWatch: (id: string, token: number) => detachWorktreeWatchMock(id, token),
  worktreeListFiles: (root: string) => worktreeListFilesMock(root),
  worktreeFileHead: (path: string) => worktreeFileHeadMock(path),
  sourceRemoteListDirectory: (...a: unknown[]) => remote.list(...a),
  sourceRemoteFileRead: (...a: unknown[]) => remote.read(...a),
  sourceRemoteFileWrite: (...a: unknown[]) => remote.write(...a),
  sourceRemoteFileInspect: (...a: unknown[]) => remote.inspect(...a),
  sourceRemoteFileHead: (...a: unknown[]) => remote.head(...a),
  sourceRemoteListFiles: (...a: unknown[]) => remote.files(...a),
  sourceRemoteWatchStart: () => Promise.resolve(1),
  sourceRemoteWatchStop: () => Promise.resolve(),
  sourceRemoteTrackedCount: () => Promise.resolve(10),
  // SLW-WDE-5: the external-editor button lazily detects editor labels — keep it
  // deterministic (empty → generic label) and off real IPC.
  detectTools: () =>
    Promise.resolve({ terminals: [], editors: [], default_terminal: "", default_editor: "" }),
}));

import { SourceEditorInset, WorktreeEditorSurface } from "./source-editor-inset";
import { openInWorktreeEditor, useWorktreeEditorRevealStore } from "../lib/open-in-worktree-editor";
import { resetDetectedEditorsCache } from "@/shared/hooks/use-detected-editors";
import { sha256Hex } from "../lib/use-file-editor";
import { warmScopes } from "@/shared/lib/warm-scopes";

const ws: Workspace = {
  id: "w1",
  kind: "story",
  cwd: "/wt",
  status: "ready",
  storyId: "SLW-WDE-2",
  storyFilePath: "/repo/story.md",
  sourceRepoPath: "/repo",
};

beforeEach(() => {
  resetDetectedEditorsCache();
  editorHandle.revealLine.mockReset();
  editorHandle.openSearch.mockReset();
  editorHandle.focus.mockReset();
  // SW-VE-12 (B4): the live-cwd override defaults to null (fall back to
  // ws.cwd) so each test opts in via `sessionCwdMock.mockReturnValue(...)`.
  // Reset at top-level so an override in one describe never leaks across
  // describe boundaries (`clearAllMocks` wouldn't restore the function).
  sessionCwdMock.mockReset();
  sessionCwdMock.mockReturnValue(null);
  sessionCwdRevisionMock.mockReset();
  sessionCwdRevisionMock.mockReturnValue(0);
  readFileMock.mockReset();
  readFileMock.mockResolvedValue("const x = 1;\n");
  writeWorktreeFileMock.mockReset();
  // A save lands on disk: later reads see what was written.
  writeWorktreeFileMock.mockImplementation(async (_p, c) => {
    readFileMock.mockResolvedValue(c);
  });
  writeHashes.length = 0;
  lazyAttachWorktreeWatchMock.mockReset();
  lazyAttachWorktreeWatchMock.mockResolvedValue(1); // attach returns a token
  detachWorktreeWatchMock.mockReset();
  detachWorktreeWatchMock.mockResolvedValue(undefined);
  worktreeListFilesMock.mockReset();
  worktreeListFilesMock.mockResolvedValue({
    paths: ["a.ts", "b.ts", "src/deep/widget.tsx"],
    truncated: false,
    branch: "story/SLW-WDE-2",
  });
  worktreeFileHeadMock.mockReset();
  worktreeFileHeadMock.mockResolvedValue(null);
  for (const fn of Object.values(remote)) fn.mockReset();
  // SLW-WDE-3: reset the per-workspace external-change signal between tests.
  useViewerStore.setState({
    worktreeChangedPath: {},
    worktreeEditorTabs: {},
    worktreeEditorDraft: {},
  });
});

/** The visible (active-tab) pane — other open tabs stay mounted but hidden. */
function activePane() {
  return within(screen.getByTestId("worktree-pane-active"));
}

/** Open the inset, pick the text file, and wait for the editable editor. */
async function openTextFile() {
  render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={vi.fn()} />);
  fireEvent.click(screen.getByTestId("pick-text"));
  return screen.findByTestId("editor-textarea");
}

describe("SourceEditorInset — read/gate (SLW-WDE-1 carried forward)", () => {
  it("opens at an empty state when no file is seeded — no read fired", () => {
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={vi.fn()} />);
    expect(screen.getByText(/pick a file/i)).toBeInTheDocument();
    expect(readFileMock).not.toHaveBeenCalled();
  });

  it("SLW-WDE-5 §D: the inset header hosts the external-editor escape hatch", () => {
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={vi.fn()} />);
    const external = screen.getByTestId("source-open-external-editor-w1");
    expect(external).toBeInTheDocument();
    expect(external).toHaveAttribute("aria-label", "Open in editor");
    // still beside the Close control, not replacing it
    expect(screen.getByTestId("source-editor-back-w1")).toBeInTheDocument();
  });

  it("opens at the seeded line — the diff's 'Open in code editor' lands on the change", async () => {
    readFileMock.mockResolvedValueOnce("a\nb\nc\n");
    render(
      <SourceEditorInset workspace={ws} seedRelPath="src/a.ts" seedLine={42} onClose={vi.fn()} />,
    );
    await screen.findByTestId("mock-code-editor");
    await waitFor(() => expect(editorHandle.revealLine).toHaveBeenCalledWith(42, 1));
  });

  it("§F: seeds the initial file from the diff selection (repo-relative → absolute)", async () => {
    render(<SourceEditorInset workspace={ws} seedRelPath="src/a.ts" onClose={vi.fn()} />);
    expect(readFileMock).toHaveBeenCalledWith("/wt/src/a.ts");
    const editor = await screen.findByTestId("mock-code-editor");
    expect(editor).toHaveAttribute("data-path", "/wt/src/a.ts");
  });

  it("AC-1: a selected text file mounts an EDITABLE editor (not read-only) with its content", async () => {
    readFileMock.mockResolvedValueOnce("export const y = 2;\n");
    const textarea = await openTextFile();
    expect(textarea).toHaveValue("export const y = 2;\n");
    // editable: the WDE-1 read-only flag is gone.
    expect(screen.getByTestId("mock-code-editor")).not.toHaveAttribute("data-readonly", "true");
  });

  it("§B: an oversized file shows a placeholder WITHOUT reading it — not editable", () => {
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={vi.fn()} />);
    fireEvent.click(screen.getByTestId("pick-large"));
    expect(screen.getByTestId("worktree-file-placeholder")).toBeInTheDocument();
    expect(screen.getByText(/too large/i)).toBeInTheDocument();
    // Says how big the file is and what the editor takes, not just "no".
    expect(screen.getByTestId("worktree-file-placeholder")).toHaveTextContent("5 MB");
    expect(screen.getByTestId("worktree-file-placeholder")).toHaveTextContent(/up to 1\.5 MB/);
    expect(readFileMock).not.toHaveBeenCalled();
    expect(screen.queryByTestId("mock-code-editor")).not.toBeInTheDocument();
  });

  it("§B: binary content (NUL byte) shows a placeholder, not an editor", async () => {
    readFileMock.mockResolvedValueOnce("\0PNG rest");
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={vi.fn()} />);
    fireEvent.click(screen.getByTestId("pick-binary"));
    expect(await screen.findByTestId("worktree-file-placeholder")).toBeInTheDocument();
    expect(screen.getByText(/binary file/i)).toBeInTheDocument();
    expect(screen.queryByTestId("mock-code-editor")).not.toBeInTheDocument();
  });

  it("AC-5 (read): a read failure shows an inline error + Retry that re-reads", async () => {
    readFileMock.mockRejectedValueOnce(new Error("EACCES"));
    render(<SourceEditorInset workspace={ws} seedRelPath="src/a.ts" onClose={vi.fn()} />);
    expect(await screen.findByTestId("worktree-file-error")).toBeInTheDocument();

    readFileMock.mockResolvedValueOnce("recovered();\n");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    const textarea = await screen.findByTestId("editor-textarea");
    expect(textarea).toHaveValue("recovered();\n");
  });
});

describe("SourceEditorInset — edit + save (SLW-WDE-2)", () => {
  it("AC-2: a dirty dot appears when the buffer diverges and clears after save", async () => {
    const textarea = await openTextFile();
    expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument();

    fireEvent.change(textarea, { target: { value: "const x = 2;\n" } });
    expect(screen.getByTestId("worktree-dirty-dot")).toBeInTheDocument();

    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    await waitFor(() => expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument());
  });

  it("AC-3: ⌘S writes the buffer to the file's absolute path via writeWorktreeFile", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "const x = 99;\n" } });
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    await waitFor(() =>
      expect(writeWorktreeFileMock).toHaveBeenCalledWith("/wt/a.ts", "const x = 99;\n"),
    );
  });

  it("a local save carries the SHA-256 of the disk text it was edited from", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "const x = 7;\n" } });
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    await waitFor(() => expect(writeWorktreeFileMock).toHaveBeenCalled());
    expect(writeHashes[0]).toBe(await sha256Hex("const x = 1;\n"));
  });

  it("AC-3: the header Save control saves through the same path", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "saved via button\n" } });
    fireEvent.click(screen.getByTestId("source-editor-save-w1"));
    await waitFor(() =>
      expect(writeWorktreeFileMock).toHaveBeenCalledWith("/wt/a.ts", "saved via button\n"),
    );
  });

  it("AC-4: a clean buffer never fires a write (⌘S or Save button)", async () => {
    await openTextFile();
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    fireEvent.click(screen.getByTestId("source-editor-save-w1"));
    await Promise.resolve();
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
  });

  it("AC-4 (§C): a save already in flight coalesces the header-button save", async () => {
    let release: () => void = () => {};
    writeWorktreeFileMock.mockImplementationOnce(
      () => new Promise<void>((r) => (release = () => r())),
    );
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "edit\n" } });
    // ⌘S starts the (pending) write, then the button fires again mid-flight.
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    fireEvent.click(screen.getByTestId("source-editor-save-w1"));
    release();
    // Same buffer both times → coalesced to ONE write, no trailing flush.
    await waitFor(() => expect(writeWorktreeFileMock).toHaveBeenCalledTimes(1));
  });

  it("AC-4 (§C): an edit made DURING an in-flight save is flushed when the write completes (trailing coalesce)", async () => {
    let releaseFirst: () => void = () => {};
    writeWorktreeFileMock
      .mockImplementationOnce(() => new Promise<void>((r) => (releaseFirst = () => r())))
      .mockResolvedValueOnce(undefined);
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "v1\n" } });
    fireEvent.click(screen.getByTestId("editor-cmd-s")); // writes v1 (pending)
    fireEvent.change(textarea, { target: { value: "v2\n" } }); // edit lands mid-flight
    // The write goes out once its lock (the base's SHA-256) is computed.
    await waitFor(() => expect(writeWorktreeFileMock).toHaveBeenCalledTimes(1));
    releaseFirst(); // v1 resolves → trailing flush writes the latest (v2)
    await waitFor(() => {
      expect(writeWorktreeFileMock).toHaveBeenCalledTimes(2);
      expect(writeWorktreeFileMock).toHaveBeenLastCalledWith("/wt/a.ts", "v2\n");
    });
    // and the buffer ends up clean.
    await waitFor(() => expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument());
  });

  it("AC-5 (save): a failed write shows an inline error, KEEPS the buffer dirty, and Retry re-saves", async () => {
    writeWorktreeFileMock.mockRejectedValueOnce(new Error("EROFS"));
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "will fail\n" } });
    fireEvent.click(screen.getByTestId("editor-cmd-s"));

    expect(await screen.findByTestId("worktree-save-error")).toBeInTheDocument();
    // edits never dropped — still dirty.
    expect(screen.getByTestId("worktree-dirty-dot")).toBeInTheDocument();

    // Retry succeeds → error + dirty clear.
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => {
      expect(screen.queryByTestId("worktree-save-error")).not.toBeInTheDocument();
      expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument();
    });
    expect(writeWorktreeFileMock).toHaveBeenLastCalledWith("/wt/a.ts", "will fail\n");
  });

  it("tabs: picking another file while dirty opens it in a NEW tab — the edit stays in its own tab", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "dirty edit\n" } });

    readFileMock.mockResolvedValueOnce("export const b = 3;\n"); // b.ts
    fireEvent.click(screen.getByTestId("pick-other"));
    // No discard prompt: nothing is being thrown away.
    expect(screen.queryByTestId("worktree-discard-confirm")).not.toBeInTheDocument();
    await waitFor(() =>
      expect(activePane().getByTestId("mock-code-editor")).toHaveAttribute("data-path", "/wt/b.ts"),
    );
    expect(activePane().getByTestId("editor-textarea")).toHaveValue("export const b = 3;\n");

    // Back to a.ts: the unsaved edit is exactly where it was.
    fireEvent.click(screen.getByTestId("worktree-tab-a.ts"));
    expect(activePane().getByTestId("editor-textarea")).toHaveValue("dirty edit\n");
    expect(screen.getByTestId("worktree-dirty-dot")).toBeInTheDocument();
  });

  it("tabs: closing a dirty tab confirms — Keep editing keeps it, Discard closes it", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "dirty edit\n" } });

    fireEvent.click(screen.getByRole("button", { name: "Close a.ts (unsaved changes)" }));
    expect(screen.getByTestId("worktree-discard-confirm")).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("worktree-discard-confirm-no"));
    expect(screen.queryByTestId("worktree-discard-confirm")).not.toBeInTheDocument();
    expect(activePane().getByTestId("editor-textarea")).toHaveValue("dirty edit\n");

    fireEvent.click(screen.getByRole("button", { name: "Close a.ts (unsaved changes)" }));
    fireEvent.click(screen.getByTestId("worktree-discard-confirm-yes"));
    expect(screen.queryByTestId("worktree-tab-a.ts")).not.toBeInTheDocument();
    expect(screen.getByTestId("worktree-empty")).toBeInTheDocument();
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
  });

  it("tabs: a restored draft discarded on close does not come back when the file reopens", async () => {
    render(
      <SourceEditorInset
        workspace={ws}
        seedRelPath="a.ts"
        restoreDrafts={{
          "a.ts": { relPath: "a.ts", buffer: "restored draft\n", lastSaved: "const x = 1;\n" },
        }}
        onClose={vi.fn()}
      />,
    );
    expect(await screen.findByTestId("editor-textarea")).toHaveValue("restored draft\n");
    fireEvent.click(screen.getByRole("button", { name: "Close a.ts (unsaved changes)" }));
    fireEvent.click(screen.getByTestId("worktree-discard-confirm-yes"));
    expect(screen.queryByTestId("worktree-tab-a.ts")).not.toBeInTheDocument();

    fireEvent.click(screen.getByTestId("pick-text"));
    await waitFor(() =>
      expect(activePane().getByTestId("editor-textarea")).toHaveValue("const x = 1;\n"),
    );
    expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument();
  });

  it("tabs: the close confirm can save instead, then closes the tab", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "keep this\n" } });
    fireEvent.click(screen.getByRole("button", { name: "Close a.ts (unsaved changes)" }));
    fireEvent.click(screen.getByTestId("worktree-discard-confirm-save"));
    await waitFor(() =>
      expect(writeWorktreeFileMock).toHaveBeenCalledWith("/wt/a.ts", "keep this\n"),
    );
    await waitFor(() => expect(screen.queryByTestId("worktree-tab-a.ts")).not.toBeInTheDocument());
  });

  it("tabs: a failed save from the close confirm keeps the tab and its edits", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "keep this\n" } });
    writeWorktreeFileMock.mockRejectedValueOnce(new Error("EROFS"));
    fireEvent.click(screen.getByRole("button", { name: "Close a.ts (unsaved changes)" }));
    await act(async () => {
      fireEvent.click(screen.getByTestId("worktree-discard-confirm-save"));
    });
    await waitFor(() => expect(writeWorktreeFileMock).toHaveBeenCalled());
    expect(await screen.findByTestId("worktree-save-error")).toBeInTheDocument();
    expect(screen.getByTestId("worktree-tab-a.ts")).toBeInTheDocument();
    expect(activePane().getByTestId("editor-textarea")).toHaveValue("keep this\n");
  });

  it("the merge shows an agent's LF rewrite of a CRLF file in CRLF", async () => {
    readFileMock.mockResolvedValueOnce("a\r\nb\r\n");
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={vi.fn()} />);
    fireEvent.click(screen.getByTestId("pick-text"));
    fireEvent.change(await screen.findByTestId("editor-textarea"), {
      target: { value: "a\r\nmine\r\n" },
    });
    readFileMock.mockResolvedValue("a\nagent\n");
    act(() => useViewerStore.getState().bumpWorktreeChangedPath(ws.id, "a.ts"));
    fireEvent.click(await screen.findByTestId("worktree-stale-resolve"));
    expect(await screen.findByTestId("mock-merge-view")).toHaveAttribute(
      "data-disk",
      "a\r\nagent\r\n",
    );
  });

  it("⌃G over an open quick open switches it to go to line", async () => {
    await openTextFile();
    const inset = screen.getByTestId("source-editor-inset-w1");
    fireEvent.keyDown(inset, { key: "p", metaKey: true });
    fireEvent.keyDown(inset, { key: "g", ctrlKey: true });
    expect(await screen.findByRole("dialog", { name: /go to line/i })).toBeInTheDocument();
  });

  it("tabs: a clean tab closes immediately and focus moves to its neighbour", async () => {
    await openTextFile();
    readFileMock.mockResolvedValueOnce("export const b = 3;\n");
    fireEvent.click(screen.getByTestId("pick-other"));
    await waitFor(() =>
      expect(activePane().getByTestId("mock-code-editor")).toHaveAttribute("data-path", "/wt/b.ts"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Close b.ts" }));
    expect(screen.queryByTestId("worktree-discard-confirm")).not.toBeInTheDocument();
    expect(activePane().getByTestId("mock-code-editor")).toHaveAttribute("data-path", "/wt/a.ts");
  });

  it("tabs: ⌘W closes the active tab; ⌃Tab cycles tabs", async () => {
    await openTextFile();
    readFileMock.mockResolvedValueOnce("export const b = 3;\n");
    fireEvent.click(screen.getByTestId("pick-other"));
    await screen.findByTestId("worktree-tab-b.ts");
    const inset = screen.getByTestId("source-editor-inset-w1");
    fireEvent.keyDown(inset, { key: "Tab", ctrlKey: true });
    expect(activePane().getByTestId("mock-code-editor")).toHaveAttribute("data-path", "/wt/a.ts");
    fireEvent.keyDown(inset, { key: "w", metaKey: true });
    expect(screen.queryByTestId("worktree-tab-a.ts")).not.toBeInTheDocument();
    expect(activePane().getByTestId("mock-code-editor")).toHaveAttribute("data-path", "/wt/b.ts");
  });

  it("AC-6: closing (Esc/back) while dirty guards; a clean buffer closes immediately", async () => {
    const onClose = vi.fn();
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={onClose} />);
    fireEvent.click(screen.getByTestId("pick-text"));
    const textarea = await screen.findByTestId("editor-textarea");

    // Clean → Esc closes.
    fireEvent.keyDown(screen.getByTestId("source-editor-inset-w1"), { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);

    // Dirty → Esc opens the confirm instead of closing.
    fireEvent.change(textarea, { target: { value: "dirty\n" } });
    fireEvent.keyDown(screen.getByTestId("source-editor-inset-w1"), { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("worktree-discard-confirm")).toBeInTheDocument();

    // Discard confirms the close.
    fireEvent.click(screen.getByTestId("worktree-discard-confirm-yes"));
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("§E: Escape stop-propagates so the overlay-hide Escape never fires underneath", async () => {
    const onClose = vi.fn();
    const outerEscape = vi.fn();
    render(
      <div onKeyDown={outerEscape}>
        <SourceEditorInset workspace={ws} seedRelPath={null} onClose={onClose} />
      </div>,
    );
    fireEvent.keyDown(screen.getByTestId("source-editor-inset-w1"), { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(outerEscape).not.toHaveBeenCalled();
  });

  it("race guard: a save resolving after another tab opens never touches that tab", async () => {
    // File A's write hangs until released — simulating a still-in-flight save.
    let releaseA: () => void = () => {};
    writeWorktreeFileMock.mockImplementationOnce(
      () => new Promise<void>((r) => (releaseA = () => r())),
    );
    readFileMock.mockResolvedValueOnce("const x = 1;\n"); // a.ts
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={vi.fn()} />);
    fireEvent.click(screen.getByTestId("pick-text"));
    const textareaA = await screen.findByTestId("editor-textarea");
    fireEvent.change(textareaA, { target: { value: "edited A\n" } });
    fireEvent.click(screen.getByTestId("editor-cmd-s")); // A's write starts (pending)

    readFileMock.mockResolvedValueOnce("export const b = 3;\n"); // b.ts
    fireEvent.click(screen.getByTestId("pick-other"));
    await waitFor(() =>
      expect(activePane().getByTestId("mock-code-editor")).toHaveAttribute("data-path", "/wt/b.ts"),
    );

    // A's write resolves — B is untouched and A reads clean.
    releaseA();
    await waitFor(() => expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument());
    expect(activePane().getByTestId("editor-textarea")).toHaveValue("export const b = 3;\n");
  });
});

describe("SourceEditorInset — external-change reload + warn (SLW-WDE-3)", () => {
  /** Bump the workspace's external-change signal for `relPath` (RELATIVE to the
   *  cwd — what the App `worktree-file-change` listener stores), wrapped in act
   *  so the render-time change handler runs. The inset re-joins it to `ws.cwd`. */
  function signalChange(relPath: string) {
    act(() => {
      useViewerStore.getState().bumpWorktreeChangedPath(ws.id, relPath);
    });
  }

  it("AC-1/AC-6: attaches on open, detaches (token-matched) on unmount", async () => {
    lazyAttachWorktreeWatchMock.mockResolvedValue(42);
    const { unmount } = render(
      <SourceEditorInset workspace={ws} seedRelPath={null} onClose={vi.fn()} />,
    );
    expect(lazyAttachWorktreeWatchMock).toHaveBeenCalledTimes(1);
    expect(lazyAttachWorktreeWatchMock).toHaveBeenCalledWith("w1", "/wt");
    expect(detachWorktreeWatchMock).not.toHaveBeenCalled();

    // Let attach resolve so the effect captures its token.
    await act(async () => {
      await Promise.resolve();
    });
    unmount();
    // Detach carries the token the attach returned (ordering-safe teardown).
    expect(detachWorktreeWatchMock).toHaveBeenCalledWith("w1", 42);
  });

  it("AC-4: clean buffer → reloads fresh disk content, no warning", async () => {
    const textarea = await openTextFile();
    expect(textarea).toHaveValue("const x = 1;\n");
    expect(readFileMock).toHaveBeenCalledTimes(1);

    // Agent writes the open file; the disk now has new content (relative signal).
    readFileMock.mockResolvedValue("const y = 2;\n");
    signalChange("a.ts");

    await waitFor(() =>
      expect(screen.getByTestId("editor-textarea")).toHaveValue("const y = 2;\n"),
    );
    expect(readFileMock).toHaveBeenCalledTimes(2); // re-read on the change
    expect(screen.queryByTestId("worktree-stale-warning")).not.toBeInTheDocument();
  });

  it("AC-5: dirty buffer → warns + BLOCKS the clobbering save (no write)", async () => {
    const textarea = await openTextFile();
    // Unsaved local edit.
    fireEvent.change(textarea, { target: { value: "my local edit\n" } });
    expect(screen.getByTestId("worktree-dirty-dot")).toBeInTheDocument();

    // Agent writes the same file underneath the dirty buffer (disk now differs
    // from what we last saw → a real conflict, not an echo).
    readFileMock.mockResolvedValue("agent wrote this\n");
    signalChange("a.ts");
    expect(await screen.findByTestId("worktree-stale-warning")).toBeInTheDocument();

    // ⌘S must NOT clobber the agent's write while the guard is armed.
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    fireEvent.click(screen.getByTestId(`source-editor-save-${ws.id}`));
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
    // Edits are retained (buffer unchanged), banner still up.
    expect(screen.getByTestId("editor-textarea")).toHaveValue("my local edit\n");
    expect(screen.getByTestId("worktree-stale-warning")).toBeInTheDocument();
  });

  it("AC-5: 'Reload (lose my edits)' takes disk + clears the guard", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "my local edit\n" } });
    readFileMock.mockResolvedValue("agent version\n");
    signalChange("a.ts");
    await screen.findByTestId("worktree-stale-warning");

    fireEvent.click(screen.getByTestId("worktree-stale-reload"));

    await waitFor(() =>
      expect(screen.getByTestId("editor-textarea")).toHaveValue("agent version\n"),
    );
    expect(screen.queryByTestId("worktree-stale-warning")).not.toBeInTheDocument();
    expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument();
    // A save now proceeds (guard cleared) since the buffer matches disk → no-op,
    // but a fresh edit would write.
    fireEvent.change(screen.getByTestId("editor-textarea"), { target: { value: "new edit\n" } });
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    await waitFor(() =>
      expect(writeWorktreeFileMock).toHaveBeenCalledWith("/wt/a.ts", "new edit\n"),
    );
  });

  it("AC-4: an unrelated change (open file unchanged on disk) is a no-op — no reload/warn", async () => {
    // A change signal bumps on ANY worktree change (path-agnostic — so a
    // multi-file agent turn can't overwrite the open file's own signal). The
    // reconcile reads the OPEN file; when disk is unchanged (=== lastSaved) it
    // does nothing visible, even under a dirty buffer.
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "editing\n" } });
    // Disk still holds the original open-file content (unchanged).
    readFileMock.mockResolvedValue("const x = 1;\n");

    signalChange("somewhere-else.ts");

    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.queryByTestId("worktree-stale-warning")).not.toBeInTheDocument();
    expect(screen.getByTestId("editor-textarea")).toHaveValue("editing\n");
  });

  it("catches the OPEN file's change even when a LATER different-file signal wins the store slot", async () => {
    // The regression this guards: agent writes the open file A then file B in one
    // turn → the single store slot ends at B, but the nonce still bumped, so the
    // reconcile re-reads A and catches its change (dirty → warn, no clobber).
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "my unsaved edit\n" } });
    // Disk for the open file A now holds the agent's write.
    readFileMock.mockResolvedValue("agent wrote A\n");
    // The signal that actually lands in the slot is for a DIFFERENT file (B) —
    // A's own signal was overwritten — but the nonce advanced.
    signalChange("some-other-file.ts");

    expect(await screen.findByTestId("worktree-stale-warning")).toBeInTheDocument();
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
  });

  it("the editor's OWN save reconciles to a no-op (disk === lastSaved), no false warning", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "saved content\n" } });
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    await waitFor(() =>
      expect(writeWorktreeFileMock).toHaveBeenCalledWith("/wt/a.ts", "saved content\n"),
    );

    // Disk now holds exactly what we wrote. The user types again, THEN our own
    // write's watcher echo arrives — reconciling against disk (=== lastSaved)
    // makes it a no-op: no false "disk changed" warning.
    readFileMock.mockResolvedValue("saved content\n");
    fireEvent.change(screen.getByTestId("editor-textarea"), {
      target: { value: "saved content + more\n" },
    });
    signalChange("a.ts");

    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.queryByTestId("worktree-stale-warning")).not.toBeInTheDocument();
  });

  it("a REAL external change right after our own save is still caught (not swallowed)", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "my save\n" } });
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    await waitFor(() =>
      expect(writeWorktreeFileMock).toHaveBeenCalledWith("/wt/a.ts", "my save\n"),
    );

    // The agent overwrites the file with DIFFERENT content immediately after —
    // content-compare catches it (a path-only echo heuristic would have eaten it).
    // Buffer is clean (=== our save) → disk wins.
    readFileMock.mockResolvedValue("agent overwrote it\n");
    signalChange("a.ts");
    await waitFor(() =>
      expect(screen.getByTestId("editor-textarea")).toHaveValue("agent overwrote it\n"),
    );
  });

  it("a reconcile whose disk equals the buffer is a no-op (own in-flight save / identical bytes)", async () => {
    const textarea = await openTextFile();
    // Dirty buffer holding X; disk coincidentally also holds X — models our own
    // write landing on disk BEFORE its `.then` advanced lastSaved (the race that
    // would otherwise false-warn on the user's own save).
    fireEvent.change(textarea, { target: { value: "same bytes\n" } });
    expect(screen.getByTestId("worktree-dirty-dot")).toBeInTheDocument();
    readFileMock.mockResolvedValue("same bytes\n");
    signalChange("a.ts");

    await act(async () => {
      await Promise.resolve();
    });
    // No false conflict; the buffer reconciles clean (buffer === lastSaved now).
    expect(screen.queryByTestId("worktree-stale-warning")).not.toBeInTheDocument();
    await waitFor(() => expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument());
  });

  it("a file that turns binary on disk under a DIRTY buffer warns (never silently drops edits)", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "my edits\n" } });
    // Agent replaces the file with binary content.
    readFileMock.mockResolvedValue("\0binary now");
    signalChange("a.ts");

    // Dirty buffer → conflict warning, NOT a silent placeholder; edits retained.
    expect(await screen.findByTestId("worktree-stale-warning")).toBeInTheDocument();
    expect(screen.queryByTestId("worktree-file-placeholder")).not.toBeInTheDocument();
    expect(screen.getByTestId("editor-textarea")).toHaveValue("my edits\n");
  });

  it("a stale pre-open signal does NOT retro-fire a reload/warn on open", async () => {
    // A change for a.ts is already in the store before the inset opens the file.
    act(() => {
      useViewerStore.getState().bumpWorktreeChangedPath(ws.id, "a.ts");
    });
    const textarea = await openTextFile();

    // Only the open read fired — no extra reload, no warning.
    expect(readFileMock).toHaveBeenCalledTimes(1);
    expect(screen.queryByTestId("worktree-stale-warning")).not.toBeInTheDocument();
    expect(textarea).toHaveValue("const x = 1;\n");
  });
});

describe("SourceEditorInset — two-writers reconciliation (SLW-WDE-4)", () => {
  /** Bump the workspace's external-change signal for `relPath` (relative to cwd). */
  function signalChange(relPath: string) {
    act(() => {
      useViewerStore.getState().bumpWorktreeChangedPath(ws.id, relPath);
    });
  }

  /** Drive to the reconcile surface: open a.ts, dirty it, have the agent diverge
   *  disk (→ stale banner), then click Resolve… to enter the merge view. */
  async function enterReconcile(bufferValue = "my local edit\n", diskValue = "agent wrote this\n") {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: bufferValue } });
    readFileMock.mockResolvedValue(diskValue);
    signalChange("a.ts");
    await screen.findByTestId("worktree-stale-warning");
    fireEvent.click(screen.getByTestId("worktree-stale-resolve"));
    return screen.findByTestId("mock-merge-view");
  }

  it("AC-1/§A: the stale banner's Resolve opens the disk-vs-buffer merge view (fresh disk vs buffer, no write)", async () => {
    const merge = await enterReconcile("my local edit\n", "agent wrote this\n");
    // diskValue = fresh disk (RO left); localValue = the user's buffer (editable right).
    expect(merge).toHaveAttribute("data-disk", "agent wrote this\n");
    expect(screen.getByTestId("merge-local")).toHaveValue("my local edit\n");
    // The plain editor is swapped out; nothing was written on entry.
    expect(screen.queryByTestId("mock-code-editor")).not.toBeInTheDocument();
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
    // The stale banner is hidden while the merge view is up (replaced by it).
    expect(screen.queryByTestId("worktree-stale-warning")).not.toBeInTheDocument();
  });

  it("AC-3: Take theirs adopts the REVIEWED disk directly (no re-read), clears the conflict, writes nothing", async () => {
    await enterReconcile("my local edit\n", "agent wrote this\n");
    const readsBefore = readFileMock.mock.calls.length;
    fireEvent.click(screen.getByTestId("worktree-reconcile-take-theirs"));

    // Back to the editor with the disk the user was LOOKING AT, clean, no write.
    await waitFor(() =>
      expect(screen.getByTestId("editor-textarea")).toHaveValue("agent wrote this\n"),
    );
    // Adopted the disk the merge showed in place; leaving the merge then checks
    // disk exactly once.
    await waitFor(() => expect(readFileMock.mock.calls.length).toBe(readsBefore + 1));
    expect(screen.queryByTestId("mock-merge-view")).not.toBeInTheDocument();
    expect(screen.queryByTestId("worktree-stale-warning")).not.toBeInTheDocument();
    expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument();
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
  });

  it("AC-3/§D: Keep mine writes the buffer — the ONE sanctioned clobber, bypassing the stale save-guard", async () => {
    await enterReconcile("my local edit\n", "agent wrote this\n");
    fireEvent.click(screen.getByTestId("worktree-reconcile-keep-mine"));

    // The sanctioned clobber fires (the plain save-guard blocks a normal ⌘S, but
    // this explicit post-diff resolution is the one permitted write).
    await waitFor(() =>
      expect(writeWorktreeFileMock).toHaveBeenCalledWith("/wt/a.ts", "my local edit\n"),
    );
    // Resolved → back to the editor, clean, no banner.
    await waitFor(() => expect(screen.queryByTestId("mock-merge-view")).not.toBeInTheDocument());
    expect(screen.queryByTestId("worktree-stale-warning")).not.toBeInTheDocument();
    expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument();
  });

  it("AC-3: hand-merge — edit the merge view and ⌘S writes the resolved result", async () => {
    await enterReconcile("my local edit\n", "agent wrote this\n");
    fireEvent.change(screen.getByTestId("merge-local"), { target: { value: "hand merged\n" } });
    fireEvent.click(screen.getByTestId("merge-cmd-s"));

    await waitFor(() =>
      expect(writeWorktreeFileMock).toHaveBeenCalledWith("/wt/a.ts", "hand merged\n"),
    );
    // and the editor returns with the resolved content, clean.
    await waitFor(() => expect(screen.getByTestId("editor-textarea")).toHaveValue("hand merged\n"));
    expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument();
  });

  it("AC-4: Cancel returns to the still-armed guard — a plain ⌘S is still blocked", async () => {
    await enterReconcile("my local edit\n", "agent wrote this\n");
    fireEvent.click(screen.getByTestId("worktree-reconcile-cancel"));

    // Back in the editor; the guard is still armed (banner returns, edits intact).
    expect(await screen.findByTestId("worktree-stale-warning")).toBeInTheDocument();
    expect(screen.getByTestId("editor-textarea")).toHaveValue("my local edit\n");
    // A plain save must still refuse to clobber.
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    fireEvent.click(screen.getByTestId("source-editor-save-w1"));
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
  });

  it("AC-5/§F: a mid-reconcile agent write is flagged; Refresh updates diskValue live — no remount, right-side edits preserved", async () => {
    await enterReconcile("my local edit\n", "agent wrote this\n");
    // User hand-edits the right (buffer) side.
    fireEvent.change(screen.getByTestId("merge-local"), { target: { value: "wip merge\n" } });

    // Agent writes the file AGAIN while we're resolving.
    readFileMock.mockResolvedValue("agent v2\n");
    signalChange("a.ts");

    // The merge's disk side is frozen: the change is only flagged...
    await screen.findByTestId("worktree-reconcile-refresh");
    expect(screen.getByTestId("mock-merge-view")).toHaveAttribute(
      "data-disk",
      "agent wrote this\n",
    );
    // ...until the user refreshes it — live, without remounting, so the
    // uncontrolled right-side edits survive.
    fireEvent.click(screen.getByTestId("worktree-reconcile-refresh"));
    await waitFor(() =>
      expect(screen.getByTestId("mock-merge-view")).toHaveAttribute("data-disk", "agent v2\n"),
    );
    expect(screen.getByTestId("merge-local")).toHaveValue("wip merge\n");
  });

  it("AC-6: a failed merge refresh surfaces inline and keeps the merge on the disk it showed", async () => {
    await enterReconcile("my local edit\n", "agent wrote this\n");
    readFileMock.mockRejectedValue(new Error("EACCES"));
    signalChange("a.ts");
    fireEvent.click(await screen.findByTestId("worktree-reconcile-refresh"));
    expect(await screen.findByTestId("worktree-reconcile-error")).toBeInTheDocument();
    expect(screen.getByTestId("mock-merge-view")).toHaveAttribute(
      "data-disk",
      "agent wrote this\n",
    );
  });

  it("Esc while reconciling backs out to the editor (guard still armed) — does not close the inset", async () => {
    const onClose = vi.fn();
    // enterReconcile renders its own inset; drive this one explicitly for onClose.
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={onClose} />);
    fireEvent.click(screen.getByTestId("pick-text"));
    const textarea = await screen.findByTestId("editor-textarea");
    fireEvent.change(textarea, { target: { value: "my local edit\n" } });
    readFileMock.mockResolvedValue("agent wrote this\n");
    signalChange("a.ts");
    await screen.findByTestId("worktree-stale-warning");
    fireEvent.click(screen.getByTestId("worktree-stale-resolve"));
    await screen.findByTestId("mock-merge-view");

    // Esc → back out of reconcile (not a close), guard re-armed.
    fireEvent.keyDown(screen.getByTestId("source-editor-inset-w1"), { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByTestId("mock-merge-view")).not.toBeInTheDocument();
    expect(await screen.findByTestId("worktree-stale-warning")).toBeInTheDocument();
  });

  it("opening another file while merging keeps the merge in its own tab (no cross-file disk)", async () => {
    await enterReconcile("my local edit\n", "agent wrote this\n");
    fireEvent.change(screen.getByTestId("merge-local"), { target: { value: "half-merged\n" } });

    readFileMock.mockResolvedValue("export const b = 3;\n");
    fireEvent.click(screen.getByTestId("pick-other"));

    // b.ts opens in the plain editor — never stranded in a.ts's merge view.
    await waitFor(() =>
      expect(activePane().getByTestId("mock-code-editor")).toHaveAttribute("data-path", "/wt/b.ts"),
    );
    expect(activePane().queryByTestId("mock-merge-view")).not.toBeInTheDocument();
    expect(activePane().getByTestId("editor-textarea")).toHaveValue("export const b = 3;\n");

    // a.ts still holds its merge, hand edits intact; nothing was written.
    fireEvent.click(screen.getByTestId("worktree-tab-a.ts"));
    expect(activePane().getByTestId("merge-local")).toHaveValue("half-merged\n");
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
  });

  it("AC-6: Resolve on a now-binary/too-large disk surfaces inline, opens no merge view (buffer intact)", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "my local edit\n" } });
    // The agent replaced the file with binary content underneath the dirty buffer.
    readFileMock.mockResolvedValue("\0binary now");
    signalChange("a.ts");
    await screen.findByTestId("worktree-stale-warning");

    fireEvent.click(screen.getByTestId("worktree-stale-resolve"));

    // Can't diff binary in the merge view → inline error, no merge, buffer intact.
    expect(await screen.findByTestId("worktree-reconcile-error")).toBeInTheDocument();
    expect(screen.queryByTestId("mock-merge-view")).not.toBeInTheDocument();
    expect(screen.getByTestId("editor-textarea")).toHaveValue("my local edit\n");
  });

  it("AC-5/§F: a mid-reconcile change to binary keeps the last disk shown + warns (no unviewable swap)", async () => {
    await enterReconcile("my local edit\n", "agent wrote this\n");
    expect(screen.getByTestId("mock-merge-view")).toHaveAttribute(
      "data-disk",
      "agent wrote this\n",
    );

    // The agent's next write turns the file binary while we're resolving.
    readFileMock.mockResolvedValue("\0binary now");
    signalChange("a.ts");
    fireEvent.click(await screen.findByTestId("worktree-reconcile-refresh"));

    expect(await screen.findByTestId("worktree-reconcile-error")).toBeInTheDocument();
    // The merge view stays up showing the LAST GOOD disk — never the binary blob.
    expect(screen.getByTestId("mock-merge-view")).toHaveAttribute(
      "data-disk",
      "agent wrote this\n",
    );
  });

  it("a failed resolution write surfaces inline (merge stays up); Cancel clears the error — no stranded banner", async () => {
    await enterReconcile("my local edit\n", "agent wrote this\n");
    writeWorktreeFileMock.mockRejectedValueOnce(new Error("EROFS"));
    fireEvent.click(screen.getByTestId("worktree-reconcile-keep-mine"));

    expect(await screen.findByTestId("worktree-reconcile-error")).toBeInTheDocument();
    // The resolution isn't lost — the merge view stays up so the user can retry.
    expect(screen.getByTestId("mock-merge-view")).toBeInTheDocument();

    // Cancel backs out AND clears the error (it must not strand in the editor).
    fireEvent.click(screen.getByTestId("worktree-reconcile-cancel"));
    expect(screen.queryByTestId("worktree-reconcile-error")).not.toBeInTheDocument();
    expect(screen.queryByTestId("mock-merge-view")).not.toBeInTheDocument();
  });

  it("resolveSave coalesces a double-fire (Keep mine racing the merge ⌘S) into ONE write", async () => {
    let release: () => void = () => {};
    writeWorktreeFileMock.mockImplementationOnce(
      () => new Promise<void>((r) => (release = () => r())),
    );
    await enterReconcile("my local edit\n", "agent wrote this\n");

    // Two resolution triggers back-to-back while the first write is in flight.
    fireEvent.click(screen.getByTestId("worktree-reconcile-keep-mine"));
    fireEvent.click(screen.getByTestId("merge-cmd-s"));
    release();

    await waitFor(() => expect(writeWorktreeFileMock).toHaveBeenCalledTimes(1));
  });

  it("Cancel preserves an in-progress hand-merge into the buffer (never silently dropped)", async () => {
    await enterReconcile("my local edit\n", "agent wrote this\n");
    // Type a partial hand-merge into the merge view's editable (right) side.
    fireEvent.change(screen.getByTestId("merge-local"), { target: { value: "half-merged\n" } });

    // Cancel backs out WITHOUT resolving — but carries the hand-merge into the
    // editor buffer (the merge view's local side is CM6-only; a silent revert
    // would drop the user's typing).
    fireEvent.click(screen.getByTestId("worktree-reconcile-cancel"));

    await waitFor(() => expect(screen.getByTestId("editor-textarea")).toHaveValue("half-merged\n"));
    // Still dirty + still guarded (cancel doesn't resolve), and nothing was written.
    expect(screen.getByTestId("worktree-dirty-dot")).toBeInTheDocument();
    expect(screen.getByTestId("worktree-stale-warning")).toBeInTheDocument();
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
  });

  it("closing a merging tab lifts the hand-merge before the discard confirm (all exits share the no-drop lift)", async () => {
    await enterReconcile("my local edit\n", "agent wrote this\n");
    fireEvent.change(screen.getByTestId("merge-local"), { target: { value: "half-merged\n" } });

    fireEvent.click(screen.getByRole("button", { name: /Close a\.ts/ }));
    expect(screen.getByTestId("worktree-discard-confirm")).toBeInTheDocument();
    expect(screen.queryByTestId("mock-merge-view")).not.toBeInTheDocument();
    // The editor holds the lifted hand-merge — NOT the pre-reconcile buffer.
    expect(screen.getByTestId("editor-textarea")).toHaveValue("half-merged\n");

    fireEvent.click(screen.getByTestId("worktree-discard-confirm-no"));
    expect(screen.getByTestId("mock-code-editor")).toHaveAttribute("data-path", "/wt/a.ts");
    expect(screen.getByTestId("editor-textarea")).toHaveValue("half-merged\n");
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
  });

  it("the in-flight resolve guard is per-file: a resolve in another tab isn't swallowed by a pending one", async () => {
    let releaseA: () => void = () => {};
    writeWorktreeFileMock.mockImplementationOnce(
      () => new Promise<void>((r) => (releaseA = () => r())),
    );
    await enterReconcile("A edit\n", "agent A\n");
    fireEvent.click(screen.getByTestId("worktree-reconcile-keep-mine")); // A resolve pending

    readFileMock.mockResolvedValue("b original\n");
    fireEvent.click(screen.getByTestId("pick-other"));
    await waitFor(() =>
      expect(activePane().getByTestId("mock-code-editor")).toHaveAttribute("data-path", "/wt/b.ts"),
    );

    fireEvent.change(activePane().getByTestId("editor-textarea"), {
      target: { value: "B edit\n" },
    });
    readFileMock.mockResolvedValue("agent B\n");
    signalChange("b.ts");
    await activePane().findByTestId("worktree-stale-warning");
    fireEvent.click(activePane().getByTestId("worktree-stale-resolve"));
    await activePane().findByTestId("mock-merge-view");
    fireEvent.click(activePane().getByTestId("worktree-reconcile-keep-mine"));

    await waitFor(() => expect(writeWorktreeFileMock).toHaveBeenCalledWith("/wt/b.ts", "B edit\n"));
    releaseA();
  });
});

// ────────────────────────────────────────────────────────────────────────────
// SLW-DV-3 — WorktreeEditorSurface: the standalone, diff-independent tile mount.
// ────────────────────────────────────────────────────────────────────────────

describe("WorktreeEditorSurface — standalone editor mount (SLW-DV-3)", () => {
  beforeEach(() => {
    editorHandle.revealLine.mockReset();
    // SW-VE-12 (B4): reset the live-cwd mock so one test's override never
    // leaks into the next — default is `null`, meaning fall back to ws.cwd.
    sessionCwdMock.mockReset();
    sessionCwdMock.mockReturnValue(null);
    useViewerStore.setState({
      worktreeEditorOpenBySession: {},
      worktreeEditorFile: {},
      worktreeEditorRevealLine: {},
      worktreeEditorTabs: {},
      worktreeEditorDraft: {},
      sourceDiffShownBySession: {},
    });
  });

  const inset = () => screen.queryByTestId("source-editor-inset-w1");

  it("AC-6: is a cheap gate — renders nothing (no read, no watch) while closed", () => {
    render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(inset()).not.toBeInTheDocument();
    expect(readFileMock).not.toHaveBeenCalled();
    expect(lazyAttachWorktreeWatchMock).not.toHaveBeenCalled();
  });

  it("AC-1: opens the editor with the diff HIDDEN (decoupled from source visibility)", () => {
    useViewerStore.setState({
      worktreeEditorOpenBySession: { "sid-ws": true },
      sourceDiffShownBySession: {},
    });
    render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(inset()).toBeInTheDocument();
  });

  it("lands on an armed line once, then forgets it (a later reopen resumes normally)", async () => {
    useViewerStore.setState({
      worktreeEditorOpenBySession: { "sid-ws": true },
      worktreeEditorFile: { w1: "src/mod.ts" },
      worktreeEditorRevealLine: { w1: { path: "src/mod.ts", line: 17 } },
    });
    render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    await screen.findByTestId("mock-code-editor");
    await waitFor(() => expect(editorHandle.revealLine).toHaveBeenCalledWith(17, 1));
    expect(useViewerStore.getState().worktreeEditorRevealLine.w1).toBeUndefined();
  });

  it("session memory: RESUMES the remembered file (worktreeEditorFile) on open", () => {
    useViewerStore.setState({
      worktreeEditorOpenBySession: { "sid-ws": true },
      worktreeEditorFile: { w1: "src/mod.ts" },
    });
    render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // The inset read the resumed (absolute) path on mount.
    expect(readFileMock).toHaveBeenCalledWith("/wt/src/mod.ts");
  });

  it("session memory: browsing to a file writes it back (repo-relative) for resume", () => {
    useViewerStore.setState({
      worktreeEditorOpenBySession: { "sid-ws": true },
      worktreeEditorFile: {},
    });
    render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // Pick a file in the (stubbed) tree — the surface reports it to the store.
    fireEvent.click(screen.getByTestId("pick-text")); // → /wt/a.ts
    expect(useViewerStore.getState().worktreeEditorFile.w1).toBe("a.ts");
  });

  it("AC-4: onClose flips the store flag closed", () => {
    useViewerStore.setState({ worktreeEditorOpenBySession: { "sid-ws": true } });
    render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    fireEvent.click(screen.getByTestId("source-editor-back-w1"));
    expect(useViewerStore.getState().worktreeEditorOpenBySession["sid-ws"]).toBe(false);
  });

  it("gates: unfocused / non-ready paints nothing (kind is no longer a gate — SW-VE-12)", () => {
    useViewerStore.setState({ worktreeEditorOpenBySession: { "sid-ws": true } });
    const { rerender } = render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused={false}
      />,
    );
    expect(inset()).not.toBeInTheDocument();

    rerender(
      <WorktreeEditorSurface
        workspace={{ ...ws, status: "creating" }}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(inset()).not.toBeInTheDocument();
  });

  // SW-VE-12: every folder-rooted kind can mount the editor — the gate was
  // `caps.sourceDiffToggle` (story-only), now `caps.sourceCapable` (every kind).
  it("SW-VE-12: mounts the editor on a `root` kind (plain-shell workspace parity)", () => {
    useViewerStore.setState({ worktreeEditorOpenBySession: { "sid-ws": true } });
    render(
      <WorktreeEditorSurface
        workspace={{ ...ws, kind: "root" }}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(inset()).toBeInTheDocument();
  });

  it("SW-VE-12: mounts the editor on a `generic` kind (user-picked folder tab)", () => {
    useViewerStore.setState({ worktreeEditorOpenBySession: { "sid-ws": true } });
    render(
      <WorktreeEditorSurface
        workspace={{ ...ws, kind: "generic" }}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(inset()).toBeInTheDocument();
  });

  it("SW-VE-12: mounts the editor on an `epic` kind (epic dir has a browsable folder)", () => {
    useViewerStore.setState({ worktreeEditorOpenBySession: { "sid-ws": true } });
    render(
      <WorktreeEditorSurface
        workspace={{ ...ws, kind: "epic" }}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(inset()).toBeInTheDocument();
  });

  // SW-VE-12 (B4): a terminal `cd` retargets the file tree. The editor
  // reads `useSessionCwd(activeTabSessionId)` and feeds its reading into
  // `effectiveWs` as a cwd override — a non-null live cwd that differs
  // from `ws.cwd` shifts the inset's rootPath to the new path. Verified
  // by watching the tauri `readFile` seam: the active-file path threads
  // through the live root, not the stale ws.cwd.
  it("B4: a non-null live sessionCwd overrides `ws.cwd` so the editor reads from the new root", async () => {
    sessionCwdMock.mockReturnValue("/wt-live");
    useViewerStore.setState({
      worktreeEditorOpenBySession: { "sid-ws": true },
      worktreeEditorFile: { w1: "src/mod.ts" },
    });
    render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    // readFile saw the LIVE root, not the stale ws.cwd ("/wt").
    await waitFor(() => expect(readFileMock).toHaveBeenCalledWith("/wt-live/src/mod.ts"));
  });

  // SW-VE-14: a `cd` MID-SESSION retargets the file tree too. The revision
  // counter is the hook's parameter: bumping it re-reads `useSessionCwd`,
  // which flows through `effectiveWs.cwd` into the inset's rootPath, and the
  // next file read lands under the new root. Covers the editor half of the
  // story's AC-1 (editor's file tree retargets on `cd`).
  it("SW-VE-14: an OSC 7 revision bump retargets the editor from the OLD cwd to the NEW cwd", async () => {
    // Pre-cd (revision 0): the live-cwd override reads as "/wt" (same as
    // ws.cwd), so the editor reads under the ws root.
    sessionCwdMock.mockImplementation((_sid, rev) => (rev > 0 ? "/wt-after-cd" : "/wt"));
    useViewerStore.setState({
      worktreeEditorOpenBySession: { "sid-ws": true },
      worktreeEditorFile: { w1: "src/mod.ts" },
    });
    const { rerender } = render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    await waitFor(() => expect(readFileMock).toHaveBeenCalledWith("/wt/src/mod.ts"));

    // Post-cd: bump the revision — the hook re-reads, `effectiveWs.cwd`
    // flips to the new path, and the next read is under the new root.
    readFileMock.mockClear();
    sessionCwdRevisionMock.mockReturnValue(1);
    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    await waitFor(() => expect(readFileMock).toHaveBeenCalledWith("/wt-after-cd/src/mod.ts"));
  });
});

// ────────────────────────────────────────────────────────────────────────────
// SLW-DV-9 — unsaved edits survive an unfocus that unmounts the editor. The
// draft lives in the store (above the tile), so it survives BOTH the split-view
// `!isFocused` gate and the 1-up whole-surface unmount. Driven through
// WorktreeEditorSurface — the real store seam — not the inset in isolation.
// ────────────────────────────────────────────────────────────────────────────

describe("WorktreeEditorSurface — unsaved edits survive unfocus (SLW-DV-9)", () => {
  beforeEach(() => {
    useViewerStore.setState({
      worktreeEditorOpenBySession: { "sid-ws": true },
      worktreeEditorFile: { w1: "a.ts" },
      worktreeEditorTabs: {},
      worktreeEditorDraft: {},
    });
  });

  it("AC-1: edits survive a split-view unfocus/refocus round-trip (!isFocused)", async () => {
    const { rerender } = render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const textarea = await screen.findByTestId("editor-textarea");
    expect(textarea).toHaveValue("const x = 1;\n");
    fireEvent.change(textarea, { target: { value: "const x = 2;\n" } });
    expect(screen.getByTestId("worktree-dirty-dot")).toBeInTheDocument();

    // Lose focus → inset unmounts, the dirty buffer is preserved into the store.
    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused={false}
      />,
    );
    expect(screen.queryByTestId("editor-textarea")).not.toBeInTheDocument();
    expect(useViewerStore.getState().worktreeEditorDraft.w1?.["a.ts"]).toEqual({
      relPath: "a.ts",
      buffer: "const x = 2;\n",
      lastSaved: "const x = 1;\n",
    });

    // Refocus → remounts and restores the edits + dirty flag intact.
    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(await screen.findByTestId("editor-textarea")).toHaveValue("const x = 2;\n");
    expect(screen.getByTestId("worktree-dirty-dot")).toBeInTheDocument();
  });

  it("keeps a background tab's unsaved draft across an unfocus/refocus round-trip", async () => {
    useViewerStore.setState({
      worktreeEditorTabs: { w1: ["b.ts", "a.ts"] },
      worktreeEditorDraft: {
        w1: { "b.ts": { relPath: "b.ts", buffer: "b edited\n", lastSaved: "const x = 1;\n" } },
      },
    });
    const surface = (focused: boolean) => (
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused={focused}
      />
    );
    const { rerender } = render(surface(true));
    expect(await screen.findByTestId("worktree-tab-b.ts")).toBeInTheDocument();
    rerender(surface(false));
    rerender(surface(true));
    expect(await screen.findByTestId("worktree-tab-b.ts")).toBeInTheDocument();
    expect(useViewerStore.getState().worktreeEditorDraft.w1?.["b.ts"]).toMatchObject({
      buffer: "b edited\n",
    });
  });

  it("AC-1: edits survive a 1-up workspace switch (the whole surface unmounts)", async () => {
    const { unmount } = render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const textarea = await screen.findByTestId("editor-textarea");
    fireEvent.change(textarea, { target: { value: "edited\n" } });

    // A 1-up switch tears down the entire tile frame, not just the !isFocused gate.
    unmount();
    expect(useViewerStore.getState().worktreeEditorDraft.w1?.["a.ts"]).toMatchObject({
      buffer: "edited\n",
    });

    // Switching back re-creates the tile from scratch — the store draft restores it.
    render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(await screen.findByTestId("editor-textarea")).toHaveValue("edited\n");
    expect(screen.getByTestId("worktree-dirty-dot")).toBeInTheDocument();
  });

  it("AC-2: disk changed while away → restore lands in the stale guard, no silent overwrite", async () => {
    const { rerender } = render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const textarea = await screen.findByTestId("editor-textarea");
    fireEvent.change(textarea, { target: { value: "my edit\n" } });
    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused={false}
      />,
    );

    // An agent writes the file while the tile is away (the watcher was detached,
    // so no change nonce fired — the restore's own read must catch this).
    readFileMock.mockResolvedValue("agent wrote\n");
    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );

    // Preserved edits are intact AND the stale guard is armed — a save is blocked.
    expect(await screen.findByTestId("worktree-stale-warning")).toBeInTheDocument();
    expect(screen.getByTestId("editor-textarea")).toHaveValue("my edit\n");
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    fireEvent.click(screen.getByTestId(`source-editor-save-${ws.id}`));
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
  });

  it("AC-3: a clean editor writes no draft and restores nothing (cheap gate kept)", async () => {
    const { rerender } = render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    await screen.findByTestId("editor-textarea"); // opened, never edited

    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused={false}
      />,
    );
    expect(useViewerStore.getState().worktreeEditorDraft.w1).toBeUndefined();

    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(await screen.findByTestId("editor-textarea")).toHaveValue("const x = 1;\n");
    expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument();
  });

  it("AC-3: saving a restored buffer clears its draft", async () => {
    const { rerender } = render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const textarea = await screen.findByTestId("editor-textarea");
    fireEvent.change(textarea, { target: { value: "const x = 5;\n" } });

    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused={false}
      />,
    );
    expect(useViewerStore.getState().worktreeEditorDraft.w1?.["a.ts"]).toMatchObject({
      buffer: "const x = 5;\n",
    });

    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    await screen.findByTestId("editor-textarea");
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    await waitFor(() =>
      expect(writeWorktreeFileMock).toHaveBeenCalledWith("/wt/a.ts", "const x = 5;\n"),
    );
    await waitFor(() => expect(useViewerStore.getState().worktreeEditorDraft.w1).toBeUndefined());
  });

  it("AC-3: discarding on close clears the draft — discarded edits don't resurface", async () => {
    render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const textarea = await screen.findByTestId("editor-textarea");
    fireEvent.change(textarea, { target: { value: "throwaway\n" } });

    // Close while dirty → discard-confirm → Discard.
    fireEvent.click(screen.getByTestId("source-editor-back-w1"));
    fireEvent.click(screen.getByTestId("worktree-discard-confirm-yes"));

    // The editor closed (open flipped false) and NO draft was persisted — a reopen
    // must not resurrect the discarded edits.
    expect(useViewerStore.getState().worktreeEditorOpenBySession["sid-ws"]).toBe(false);
    await waitFor(() => expect(useViewerStore.getState().worktreeEditorDraft.w1).toBeUndefined());
  });

  it("AC-1: toggling closed (⌘⇧C / chrome) preserves a dirty buffer — unlike an X-button discard", async () => {
    const { rerender } = render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const textarea = await screen.findByTestId("editor-textarea");
    fireEvent.change(textarea, { target: { value: "wip\n" } });

    // The ⌘⇧C shortcut + the chrome "Code editor" button close via the store
    // toggle, NOT the inset's X/Esc discard path. A visibility toggle should
    // preserve unsaved work (never the pre-DV-9 silent drop), so the draft is kept.
    act(() =>
      useViewerStore.getState().toggleWorktreeEditorOpen({
        sessionId: "sid-ws",
        workspaceId: "w1",
        teamKey: "team-a",
        open: false,
      }),
    );
    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(screen.queryByTestId("editor-textarea")).not.toBeInTheDocument();
    expect(useViewerStore.getState().worktreeEditorDraft.w1?.["a.ts"]).toMatchObject({
      buffer: "wip\n",
    });

    // Toggling back open restores the edits.
    act(() =>
      useViewerStore.getState().toggleWorktreeEditorOpen({
        sessionId: "sid-ws",
        workspaceId: "w1",
        teamKey: "team-a",
        open: true,
      }),
    );
    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    expect(await screen.findByTestId("editor-textarea")).toHaveValue("wip\n");
    expect(screen.getByTestId("worktree-dirty-dot")).toBeInTheDocument();
  });

  it("AC-2: restore where disk already equals the edited buffer resolves clean (no spurious stale)", async () => {
    const { rerender } = render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const textarea = await screen.findByTestId("editor-textarea");
    fireEvent.change(textarea, { target: { value: "agreed\n" } });
    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused={false}
      />,
    );

    // While away, an agent independently wrote the EXACT content the user typed.
    readFileMock.mockResolvedValue("agreed\n");
    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );

    expect(await screen.findByTestId("editor-textarea")).toHaveValue("agreed\n");
    // No conflict: disk === buffer, so the buffer reads clean and no stale banner.
    await waitFor(() => expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument());
    expect(screen.queryByTestId("worktree-stale-warning")).not.toBeInTheDocument();
  });

  it("AC-2: a failed restore read leaves disk unchecked — save is blocked and says why", async () => {
    const { rerender } = render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const textarea = await screen.findByTestId("editor-textarea");
    fireEvent.change(textarea, { target: { value: "unsaved\n" } });
    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused={false}
      />,
    );

    // The restore read fails — we can't verify disk, so we must not allow a plain
    // save (it could clobber a change made while away).
    readFileMock.mockRejectedValue(new Error("EAGAIN"));
    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );

    expect(await screen.findByTestId("editor-textarea")).toHaveValue("unsaved\n"); // edits kept
    await waitFor(() => expect(readFileMock).toHaveBeenCalled());
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    // The save waits for a check of disk, which fails again: no write, a reason.
    expect(await screen.findByTestId("worktree-save-error")).toHaveTextContent(/check the file/i);
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
  });

  it("AC-2: disk turned binary while away → restore arms the guard, edits kept", async () => {
    const { rerender } = render(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );
    const textarea = await screen.findByTestId("editor-textarea");
    fireEvent.change(textarea, { target: { value: "text edit\n" } });
    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused={false}
      />,
    );

    // Disk replaced with binary content while away (a flip past the viewable gate).
    readFileMock.mockResolvedValue("\0PNG binary");
    rerender(
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused
      />,
    );

    expect(await screen.findByTestId("worktree-stale-warning")).toBeInTheDocument();
    expect(screen.getByTestId("editor-textarea")).toHaveValue("text edit\n");
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
  });
});

// ────────────────────────────────────────────────────────────────────────────
// The full-editor chrome: quick open, go to line, keyboard ownership, the
// status bar — and the same surface on a REMOTE worktree.
// ────────────────────────────────────────────────────────────────────────────

describe("SourceEditorInset — workbench chrome", () => {
  it("⌘P opens quick open over the worktree's files; Enter opens the match in a tab", async () => {
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={vi.fn()} />);
    fireEvent.keyDown(screen.getByTestId("source-editor-inset-w1"), { key: "p", metaKey: true });
    const input = await screen.findByTestId("worktree-quick-open-input");
    await waitFor(() => expect(worktreeListFilesMock).toHaveBeenCalledWith("/wt"));
    fireEvent.change(input, { target: { value: "widget" } });
    expect(await screen.findByRole("option", { name: /widget\.tsx/ })).toBeInTheDocument();
    fireEvent.keyDown(input, { key: "Enter" });
    expect(readFileMock).toHaveBeenCalledWith("/wt/src/deep/widget.tsx");
    expect(screen.queryByTestId("worktree-quick-open")).not.toBeInTheDocument();
    expect(await screen.findByTestId("worktree-tab-src/deep/widget.tsx")).toBeInTheDocument();
  });

  it("Escape in the palette closes only the palette, never the editor", async () => {
    const onClose = vi.fn();
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={onClose} />);
    fireEvent.keyDown(screen.getByTestId("source-editor-inset-w1"), { key: "p", metaKey: true });
    const input = await screen.findByTestId("worktree-quick-open-input");
    fireEvent.keyDown(input, { key: "Escape" });
    expect(screen.queryByTestId("worktree-quick-open")).not.toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("⌃G opens the palette in go-to-line mode", async () => {
    await openTextFile();
    fireEvent.keyDown(screen.getByTestId("source-editor-inset-w1"), { key: "g", ctrlKey: true });
    const input = await screen.findByTestId("worktree-quick-open-input");
    expect(input).toHaveValue(":");
    expect(screen.getByRole("dialog", { name: "Go to line" })).toBeInTheDocument();
  });

  it("Escape typed inside the code never closes the editor", async () => {
    const onClose = vi.fn();
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={onClose} />);
    fireEvent.click(screen.getByTestId("pick-text"));
    const textarea = await screen.findByTestId("editor-textarea");
    fireEvent.keyDown(textarea, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
  });

  it("the status bar shows where the file lives, the branch, and its save state", async () => {
    await openTextFile();
    expect(screen.getByTestId("worktree-status-location")).toHaveTextContent("This Mac");
    expect(await screen.findByText("story/SLW-WDE-2")).toBeInTheDocument();
    expect(await screen.findByTestId("worktree-status-save")).toHaveTextContent("Saved");
    fireEvent.change(screen.getByTestId("editor-textarea"), { target: { value: "changed\n" } });
    await waitFor(() =>
      expect(screen.getByTestId("worktree-status-save")).toHaveTextContent("Unsaved"),
    );
  });

  it("an untracked file (no committed version) never claims 'Unchanged since commit'", async () => {
    worktreeFileHeadMock.mockResolvedValue(null);
    readFileMock.mockResolvedValue("one\ntwo\n");
    await openTextFile();
    await waitFor(() => expect(worktreeFileHeadMock).toHaveBeenCalled());
    await new Promise((r) => setTimeout(r, 300)); // past the summary's debounce
    expect(screen.queryByText("Unchanged since commit")).not.toBeInTheDocument();
    expect(screen.queryByTestId("worktree-status-changes")).not.toBeInTheDocument();
  });

  it("a tracked file's summary still counts its changes", async () => {
    worktreeFileHeadMock.mockResolvedValue("const x = 1;\n");
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "const x = 1;\nmore\n" } });
    await waitFor(() =>
      expect(screen.getByTestId("worktree-status-changes")).toHaveTextContent("+1"),
    );
  });

  it("closing the editor with several dirty tabs offers Save all", async () => {
    const onClose = vi.fn();
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={onClose} />);
    fireEvent.click(screen.getByTestId("pick-text"));
    fireEvent.change(await screen.findByTestId("editor-textarea"), { target: { value: "A2\n" } });
    readFileMock.mockResolvedValueOnce("b\n");
    fireEvent.click(screen.getByTestId("pick-other"));
    await waitFor(() =>
      expect(activePane().getByTestId("mock-code-editor")).toHaveAttribute("data-path", "/wt/b.ts"),
    );
    fireEvent.change(activePane().getByTestId("editor-textarea"), { target: { value: "B2\n" } });

    fireEvent.click(screen.getByTestId("source-editor-back-w1"));
    expect(screen.getByTestId("worktree-discard-confirm")).toHaveTextContent(
      "2 files have unsaved changes",
    );
    fireEvent.click(screen.getByTestId("worktree-discard-confirm-save"));
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(writeWorktreeFileMock).toHaveBeenCalledWith("/wt/a.ts", "A2\n");
    expect(writeWorktreeFileMock).toHaveBeenCalledWith("/wt/b.ts", "B2\n");
  });
});

describe("SourceEditorInset — remote worktree", () => {
  const remoteWs: Workspace = {
    id: "w9",
    kind: "story",
    cwd: "",
    status: "ready",
    storyId: "SLW-R-1",
    storyFilePath: "/repo/story.md",
    sourceRepoPath: "/repo",
    location: {
      kind: "machine",
      machineId: "m-1",
      requestId: "r-1",
      handle: "h-1",
      cwd: "/home/dev/wt",
      branch: "story/remote",
    },
  };
  const enc = (t: string) => btoa(String.fromCharCode(...new TextEncoder().encode(t)));

  beforeEach(() => {
    remote.files.mockResolvedValue({
      paths: ["src/a.ts"],
      truncated: false,
      includesUntracked: true,
    });
    remote.read.mockResolvedValue({
      base64: enc("remote text\n"),
      observation: { hash: "aa", mtimeNs: 0, size: 12, mode: 420 },
      truncated: false,
    });
    remote.head.mockResolvedValue({ base64: enc("remote text\n"), supported: true });
    remote.write.mockResolvedValue({ observation: { hash: "bb", mtimeNs: 1, size: 3, mode: 420 } });
    remote.inspect.mockResolvedValue({ hash: "aa", mtimeNs: 0, size: 12, mode: 420 });
  });

  it("opens, edits and saves a file on the machine with the hash lock", async () => {
    render(<SourceEditorInset workspace={remoteWs} seedRelPath="src/a.ts" onClose={vi.fn()} />);
    const textarea = await screen.findByTestId("editor-textarea");
    expect(remote.read).toHaveBeenCalledWith("m-1", "h-1", "src/a.ts");
    expect(readFileMock).not.toHaveBeenCalled();
    expect(textarea).toHaveValue("remote text\n");
    expect(screen.getByTestId("worktree-status-location")).toHaveTextContent("m-1");
    expect(await screen.findByText("story/remote")).toBeInTheDocument();
    // No "open in my local editor" for a directory that isn't on this Mac.
    expect(screen.queryByTestId("source-open-external-editor-w9")).not.toBeInTheDocument();

    fireEvent.change(textarea, { target: { value: "edited remotely\n" } });
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    await waitFor(() => expect(remote.write).toHaveBeenCalled());
    const [machineId, handle, rel, payload, expected] = remote.write.mock.calls[0]!;
    expect([machineId, handle, rel, expected]).toEqual(["m-1", "h-1", "src/a.ts", "aa"]);
    expect(atob(payload as string)).toBe("edited remotely\n");
    await waitFor(() => expect(screen.queryByTestId("worktree-dirty-dot")).not.toBeInTheDocument());
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
  });

  it("a remote Conflict on save opens the conflict guard instead of overwriting", async () => {
    remote.write.mockRejectedValue({ kind: "conflict", observation: { hash: "cc" } });
    render(<SourceEditorInset workspace={remoteWs} seedRelPath="src/a.ts" onClose={vi.fn()} />);
    const textarea = await screen.findByTestId("editor-textarea");
    fireEvent.change(textarea, { target: { value: "mine\n" } });
    remote.read.mockResolvedValue({
      base64: enc("theirs\n"),
      observation: { hash: "cc", mtimeNs: 2, size: 7, mode: 420 },
      truncated: false,
    });
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    expect(await screen.findByTestId("worktree-stale-warning")).toBeInTheDocument();
    expect(screen.getByTestId("editor-textarea")).toHaveValue("mine\n");
    expect(remote.write).toHaveBeenCalledTimes(1);
  });

  it("says so in the status bar when the machine is offline, and names quick open's failure", async () => {
    useMachinesStore.setState({
      byTeam: {
        t: [
          {
            machineId: "m-1",
            kind: "ssh",
            label: "studio",
            scope: null,
            declaration: null,
            binding: null,
            canPublish: true,
            status: { state: "disconnected" },
            attention: 0,
            capabilities: { agentCallbacks: true },
          },
        ],
      },
    });
    remote.files.mockRejectedValue({ kind: "disconnected" });
    try {
      render(<SourceEditorInset workspace={remoteWs} seedRelPath={null} onClose={vi.fn()} />);
      const location = screen.getByTestId("worktree-status-location");
      expect(location).toHaveTextContent("studio");
      expect(location).toHaveTextContent(/offline/);
      fireEvent.keyDown(screen.getByTestId("source-editor-inset-w9"), { key: "p", metaKey: true });
      expect(await screen.findByText(/Can’t reach the remote machine/)).toBeInTheDocument();
    } finally {
      useMachinesStore.setState({ byTeam: {} });
    }
  });

  it("quick open lists the machine's files", async () => {
    render(<SourceEditorInset workspace={remoteWs} seedRelPath={null} onClose={vi.fn()} />);
    fireEvent.keyDown(screen.getByTestId("source-editor-inset-w9"), { key: "p", metaKey: true });
    await screen.findByTestId("worktree-quick-open-input");
    await waitFor(() => expect(remote.files).toHaveBeenCalledWith("m-1", "h-1"));
    expect(await screen.findByRole("option", { name: /a\.ts/ })).toBeInTheDocument();
  });
});

describe("openInWorktreeEditor — the open-at-line API other tools call", () => {
  beforeEach(() => {
    useViewerStore.setState({
      worktreeEditorOpenBySession: {},
      worktreeEditorFile: {},
      worktreeEditorTabs: {},
      worktreeEditorDraft: {},
    });
    useWorktreeEditorRevealStore.setState({ requests: {} });
  });

  const surface = () => (
    <WorktreeEditorSurface workspace={ws} teamKey="team-a" activeTabSessionId="sid-ws" isFocused />
  );

  it("opens a closed editor on the file and jumps to the line once it has loaded", async () => {
    let resolveRead: (text: string) => void = () => {};
    readFileMock.mockImplementation(() => new Promise<string>((r) => (resolveRead = r)));
    render(surface());
    act(() =>
      openInWorktreeEditor({
        sessionId: "sid-ws",
        workspaceId: "w1",
        teamKey: "team-a",
        path: "src/mod.ts",
        line: 42,
        column: 7,
      }),
    );
    expect(await screen.findByTestId("source-editor-inset-w1")).toBeInTheDocument();
    expect(readFileMock).toHaveBeenCalledWith("/wt/src/mod.ts");
    // A slow read (a remote machine) never loses the jump.
    await new Promise((r) => setTimeout(r, 2500));
    expect(editorHandle.revealLine).not.toHaveBeenCalled();
    await act(async () => resolveRead("line\n".repeat(60)));
    await waitFor(() => expect(editorHandle.revealLine).toHaveBeenCalledWith(42, 7));
    expect(useWorktreeEditorRevealStore.getState().requests.w1).toBeUndefined();
  }, 10_000);

  it("one tab per file however the path is spelled (./, //, leading /)", async () => {
    useViewerStore.setState({
      worktreeEditorOpenBySession: { "sid-ws": true },
      worktreeEditorFile: { w1: "src/a.ts" },
    });
    render(surface());
    await screen.findByTestId("editor-textarea");
    for (const path of ["./src/a.ts", "src//a.ts", "/src/./a.ts"]) {
      act(() =>
        openInWorktreeEditor({ sessionId: "sid-ws", workspaceId: "w1", teamKey: "team-a", path }),
      );
    }
    await waitFor(() =>
      expect(useWorktreeEditorRevealStore.getState().requests.w1).toBeUndefined(),
    );
    expect(screen.getByTestId("worktree-tab-src/a.ts")).toBeInTheDocument();
    expect(screen.getAllByRole("tab")).toHaveLength(1);
    expect(useViewerStore.getState().worktreeEditorFile.w1).toBe("src/a.ts");
  });

  it("brings an already-open editor to another file at a line, as a new tab", async () => {
    useViewerStore.setState({
      worktreeEditorOpenBySession: { "sid-ws": true },
      worktreeEditorFile: { w1: "a.ts" },
    });
    render(surface());
    await screen.findByTestId("editor-textarea");
    act(() =>
      openInWorktreeEditor({
        sessionId: "sid-ws",
        workspaceId: "w1",
        teamKey: "team-a",
        path: "b.ts",
        line: 3,
      }),
    );
    await waitFor(() => expect(editorHandle.revealLine).toHaveBeenCalledWith(3, 1));
    expect(screen.getByTestId("worktree-tab-a.ts")).toBeInTheDocument();
    expect(screen.getByTestId("worktree-tab-b.ts")).toHaveAttribute("aria-selected", "true");
  });
});

describe("SourceEditorInset — keyboard ownership", () => {
  it("a key CodeMirror handled inside the code never also reaches the app's shortcuts", async () => {
    const textarea = await openTextFile();
    let editorHandles = true;
    const cm = (e: KeyboardEvent) => {
      if (editorHandles) e.preventDefault();
    };
    textarea.addEventListener("keydown", cm);
    const app = vi.fn();
    document.addEventListener("keydown", app);
    try {
      // ⌘] indents in the code; the app's forward must not also run.
      fireEvent.keyDown(textarea, { key: "]", metaKey: true });
      expect(app).not.toHaveBeenCalled();
      // A key the editor leaves alone (⌘1, the workspace jump) still reaches the app.
      editorHandles = false;
      fireEvent.keyDown(textarea, { key: "1", metaKey: true });
      expect(app).toHaveBeenCalledTimes(1);
    } finally {
      textarea.removeEventListener("keydown", cm);
      document.removeEventListener("keydown", app);
    }
  });

  it("the unsaved-changes prompt takes focus on its Save button; Escape returns to the code", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "edited\n" } });
    fireEvent.keyDown(textarea, { key: "w", metaKey: true });
    const save = await screen.findByTestId("worktree-discard-confirm-save");
    await waitFor(() => expect(save).toHaveFocus());
    fireEvent.keyDown(save, { key: "Escape" });
    expect(screen.queryByTestId("worktree-discard-confirm")).not.toBeInTheDocument();
  });
});

describe("SourceEditorInset — no-drop rules under races (review round 3)", () => {
  async function openTwoTabsWithADirty() {
    const onClose = vi.fn();
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={onClose} />);
    fireEvent.click(screen.getByTestId("pick-text"));
    fireEvent.change(await screen.findByTestId("editor-textarea"), { target: { value: "A2\n" } });
    readFileMock.mockResolvedValueOnce("b\n");
    fireEvent.click(screen.getByTestId("pick-other"));
    await waitFor(() => expect(activePane().getByTestId("editor-textarea")).toHaveValue("b\n"));
    return onClose;
  }

  it("Discard on the close-editor prompt asks again about a file edited while it was up", async () => {
    const onClose = await openTwoTabsWithADirty();
    fireEvent.click(screen.getByTestId("source-editor-back-w1"));
    expect(screen.getByTestId("worktree-discard-confirm")).toHaveTextContent("a.ts has unsaved");
    fireEvent.change(activePane().getByTestId("editor-textarea"), { target: { value: "B2\n" } });
    fireEvent.click(screen.getByTestId("worktree-discard-confirm-yes"));
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByTestId("worktree-discard-confirm")).toHaveTextContent(
      "2 files have unsaved changes",
    );
  });

  it("Save on the close-editor prompt asks about a file edited while it was up instead of closing over it", async () => {
    const onClose = await openTwoTabsWithADirty();
    fireEvent.click(screen.getByTestId("source-editor-back-w1"));
    fireEvent.change(activePane().getByTestId("editor-textarea"), { target: { value: "B2\n" } });
    fireEvent.click(screen.getByTestId("worktree-discard-confirm-save"));
    await waitFor(() => expect(writeWorktreeFileMock).toHaveBeenCalledWith("/wt/a.ts", "A2\n"));
    await waitFor(() =>
      expect(screen.getByTestId("worktree-discard-confirm")).toHaveTextContent("b.ts has unsaved"),
    );
    expect(onClose).not.toHaveBeenCalled();
    expect(writeWorktreeFileMock).not.toHaveBeenCalledWith("/wt/b.ts", "B2\n");
  });

  it("a tab opened during a slow save-then-close survives the close", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "keep this\n" } });
    let resolveWrite: () => void = () => {};
    writeWorktreeFileMock.mockImplementationOnce(
      () => new Promise<void>((r) => (resolveWrite = r)),
    );
    fireEvent.click(screen.getByRole("button", { name: "Close a.ts (unsaved changes)" }));
    fireEvent.click(screen.getByTestId("worktree-discard-confirm-save"));
    readFileMock.mockResolvedValueOnce("b\n");
    fireEvent.click(screen.getByTestId("pick-other"));
    expect(await screen.findByTestId("worktree-tab-b.ts")).toBeInTheDocument();
    await act(async () => resolveWrite());
    await waitFor(() => expect(screen.queryByTestId("worktree-tab-a.ts")).not.toBeInTheDocument());
    expect(screen.getByTestId("worktree-tab-b.ts")).toBeInTheDocument();
  });

  it("⌘W with no file open closes the editor instead of being swallowed", () => {
    const onClose = vi.fn();
    render(<SourceEditorInset workspace={ws} seedRelPath={null} onClose={onClose} />);
    fireEvent.keyDown(screen.getByTestId("source-editor-inset-w1"), { key: "w", metaKey: true });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("⇧⌘S from the chrome does not save", async () => {
    const textarea = await openTextFile();
    fireEvent.change(textarea, { target: { value: "edited\n" } });
    fireEvent.keyDown(screen.getByTestId("source-editor-inset-w1"), {
      key: "s",
      metaKey: true,
      shiftKey: true,
    });
    await act(async () => {});
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
  });

  it("⌥Z inside the code types a layout's letter (ż) but toggles wrap where it types Ω", async () => {
    const textarea = await openTextFile();
    const polish = fireEvent.keyDown(textarea, { key: "ż", code: "KeyZ", altKey: true });
    expect(polish).toBe(true); // not prevented: the letter is typed
    const us = fireEvent.keyDown(textarea, { key: "Ω", code: "KeyZ", altKey: true });
    expect(us).toBe(false);
  });
});

describe("unsaved work survives a pane unmount (review round 3)", () => {
  beforeEach(() => {
    useViewerStore.setState({
      worktreeEditorOpenBySession: { "sid-ws": true },
      worktreeEditorFile: { w1: "a.ts" },
      worktreeEditorTabs: {},
      worktreeEditorDraft: {},
    });
  });

  it("an unfocus mid-merge keeps the hand-merged side as the draft", async () => {
    const surface = (isFocused: boolean) => (
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused={isFocused}
      />
    );
    const { rerender } = render(surface(true));
    fireEvent.change(await screen.findByTestId("editor-textarea"), {
      target: { value: "mine\n" },
    });
    readFileMock.mockResolvedValue("agent\n");
    act(() => useViewerStore.getState().bumpWorktreeChangedPath(ws.id, "a.ts"));
    fireEvent.click(await screen.findByTestId("worktree-stale-resolve"));
    fireEvent.change(await screen.findByTestId("merge-local"), {
      target: { value: "hand merged\n" },
    });
    rerender(surface(false));
    expect(useViewerStore.getState().worktreeEditorDraft.w1?.["a.ts"]?.buffer).toBe(
      "hand merged\n",
    );
  });

  it("a remote backend that drops out and returns restores the dirty buffer, not clean disk", async () => {
    const enc = (t: string) => btoa(String.fromCharCode(...new TextEncoder().encode(t)));
    remote.read.mockResolvedValue({
      base64: enc("remote text\n"),
      observation: { hash: "aa", mtimeNs: 0, size: 12, mode: 420 },
      truncated: false,
    });
    remote.head.mockResolvedValue({ base64: enc("remote text\n"), supported: true });
    remote.files.mockResolvedValue({ paths: [], truncated: false, includesUntracked: true });
    remote.inspect.mockResolvedValue({ hash: "aa", mtimeNs: 0, size: 12, mode: 420 });
    const location = {
      kind: "machine" as const,
      machineId: "m-1",
      requestId: "r-1",
      handle: "h-1",
      cwd: "/home/dev/wt",
      branch: "story/remote",
    };
    const remoteWs: Workspace = { ...ws, id: "w9", cwd: "", location };
    const drafts: Record<string, unknown> = {};
    const persist = vi.fn((rel: string, d: unknown) => {
      if (d) drafts[rel] = d;
      else delete drafts[rel];
    });
    const inset = (w: Workspace) => (
      <SourceEditorInset
        workspace={w}
        seedRelPath="src/a.ts"
        onPersistDraft={persist}
        onClose={vi.fn()}
      />
    );
    const { rerender } = render(inset(remoteWs));
    fireEvent.change(await screen.findByTestId("editor-textarea"), {
      target: { value: "edited\n" },
    });
    rerender(inset({ ...remoteWs, location: { ...location, handle: "" } }));
    expect(screen.queryByTestId("editor-textarea")).not.toBeInTheDocument();
    rerender(inset(remoteWs));
    expect(await screen.findByTestId("editor-textarea")).toHaveValue("edited\n");
    await waitFor(() => expect(screen.getByTestId("worktree-dirty-dot")).toBeInTheDocument());
    expect(drafts["src/a.ts"]).toMatchObject({ buffer: "edited\n" });
  });
});

describe("review round 4", () => {
  const enc = (t: string) => btoa(String.fromCharCode(...new TextEncoder().encode(t)));
  const location = {
    kind: "machine" as const,
    machineId: "m-1",
    requestId: "r-1",
    handle: "h-1",
    cwd: "/home/dev/wt",
    branch: "story/remote",
  };
  const remoteWs: Workspace = { ...ws, id: "w9", cwd: "", location };
  beforeEach(() => {
    remote.read.mockResolvedValue({
      base64: enc("remote text\n"),
      observation: { hash: "aa", mtimeNs: 0, size: 12, mode: 420 },
      truncated: false,
    });
    remote.head.mockResolvedValue({ base64: enc("remote text\n"), supported: true });
    remote.files.mockResolvedValue({ paths: [], truncated: false, includesUntracked: true });
    remote.inspect.mockResolvedValue({ hash: "aa", mtimeNs: 0, size: 12, mode: 420 });
    useViewerStore.setState({
      worktreeEditorOpenBySession: { "sid-ws": true },
      worktreeEditorFile: { w1: "a.ts" },
      worktreeEditorTabs: {},
      worktreeEditorDraft: {},
    });
  });

  it("an unfocus mid-merge keeps a hand-merge equal to the old baseline, dirty against theirs", async () => {
    const surface = (isFocused: boolean) => (
      <WorktreeEditorSurface
        workspace={ws}
        teamKey="team-a"
        activeTabSessionId="sid-ws"
        isFocused={isFocused}
      />
    );
    const { rerender } = render(surface(true));
    fireEvent.change(await screen.findByTestId("editor-textarea"), {
      target: { value: "mine\n" },
    });
    readFileMock.mockResolvedValue("agent\n");
    act(() => useViewerStore.getState().bumpWorktreeChangedPath(ws.id, "a.ts"));
    fireEvent.click(await screen.findByTestId("worktree-stale-resolve"));
    fireEvent.change(await screen.findByTestId("merge-local"), {
      target: { value: "const x = 1;\n" },
    });
    rerender(surface(false));
    expect(useViewerStore.getState().worktreeEditorDraft.w1?.["a.ts"]).toMatchObject({
      buffer: "const x = 1;\n",
      lastSaved: "agent\n",
    });
  });

  it("⌥Z inside the code toggles wrap where the layout types a symbol (¥), not a letter", async () => {
    const textarea = await openTextFile();
    expect(fireEvent.keyDown(textarea, { key: "¥", code: "KeyZ", altKey: true })).toBe(false);
  });

  it("tabs restored before the machine reports the worktree root re-anchor when it arrives", async () => {
    const onTabsChange = vi.fn();
    const inset = (w: Workspace) => (
      <SourceEditorInset
        workspace={w}
        seedRelPath={null}
        initialTabs={["src/a.ts"]}
        onTabsChange={onTabsChange}
        onClose={vi.fn()}
      />
    );
    const { rerender } = render(inset({ ...remoteWs, location: { ...location, cwd: undefined } }));
    expect(screen.queryByTestId("editor-textarea")).not.toBeInTheDocument();
    rerender(inset(remoteWs));
    expect(await screen.findByTestId("editor-textarea")).toHaveValue("remote text\n");
    expect(remote.read).toHaveBeenCalledWith("m-1", "h-1", "src/a.ts");
    for (const [rels] of onTabsChange.mock.calls) expect(rels).toEqual(["src/a.ts"]);
  });

  it("closing a dirty tab whose pane is down (backend gone) still asks first", async () => {
    const persist = vi.fn();
    const inset = (w: Workspace) => (
      <SourceEditorInset
        workspace={w}
        seedRelPath="src/a.ts"
        onPersistDraft={persist}
        onClose={vi.fn()}
      />
    );
    const { rerender } = render(inset(remoteWs));
    fireEvent.change(await screen.findByTestId("editor-textarea"), {
      target: { value: "edited\n" },
    });
    rerender(inset({ ...remoteWs, location: { ...location, handle: "" } }));
    expect(screen.queryByTestId("editor-textarea")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /^Close a\.ts/ }));
    expect(screen.getByTestId("worktree-discard-confirm")).toBeInTheDocument();
    // Save can't reach the machine: the tab stays, its draft kept.
    await act(async () => {
      fireEvent.click(screen.getByTestId("worktree-discard-confirm-save"));
    });
    expect(screen.getByTestId("worktree-tab-src/a.ts")).toBeInTheDocument();
    expect(persist).not.toHaveBeenLastCalledWith("src/a.ts", null);
  });

  it("closing the editor with a tab whose pane is down asks first; Discard drops its draft", async () => {
    const persist = vi.fn();
    const onClose = vi.fn();
    const inset = (w: Workspace) => (
      <SourceEditorInset
        workspace={w}
        seedRelPath="src/a.ts"
        onPersistDraft={persist}
        onClose={onClose}
      />
    );
    const { rerender } = render(inset(remoteWs));
    fireEvent.change(await screen.findByTestId("editor-textarea"), {
      target: { value: "edited\n" },
    });
    rerender(inset({ ...remoteWs, location: { ...location, handle: "" } }));
    // The open tab says why it shows nothing.
    expect(screen.getByTestId("worktree-waiting")).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("source-editor-back-w9"));
    expect(screen.getByTestId("worktree-discard-confirm")).toBeInTheDocument();
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("worktree-discard-confirm-yes"));
    expect(onClose).toHaveBeenCalled();
    expect(persist).toHaveBeenLastCalledWith("src/a.ts", null);
  });
});

describe("review round 5", () => {
  const enc = (t: string) => btoa(String.fromCharCode(...new TextEncoder().encode(t)));
  const location = {
    kind: "machine" as const,
    machineId: "m-1",
    requestId: "r-1",
    handle: "h-1",
    cwd: "/home/dev/wt",
    branch: "story/remote",
  };
  const remoteWs: Workspace = { ...ws, id: "w9", cwd: "", location };
  const draft = { relPath: "src/a.ts", buffer: "kept edit\n", lastSaved: "remote text\n" };
  beforeEach(() => {
    remote.read.mockResolvedValue({
      base64: enc("remote text\n"),
      observation: { hash: "aa", mtimeNs: 0, size: 12, mode: 420 },
      truncated: false,
    });
    remote.head.mockResolvedValue({ base64: enc("remote text\n"), supported: true });
    remote.files.mockResolvedValue({ paths: [], truncated: false, includesUntracked: true });
    remote.inspect.mockResolvedValue({ hash: "aa", mtimeNs: 0, size: 12, mode: 420 });
    useWorktreeEditorRevealStore.setState({ requests: {} });
  });

  it("opening an 11th tab while the panes are down never evicts a tab holding a kept draft", async () => {
    const tabs = ["src/a.ts", ...Array.from({ length: 9 }, (_, i) => `f${i}.ts`)];
    const onTabsChange = vi.fn();
    render(
      <SourceEditorInset
        workspace={{ ...remoteWs, location: { ...location, handle: "" } }}
        seedRelPath={null}
        initialTabs={tabs}
        restoreDrafts={{ "src/a.ts": draft }}
        onTabsChange={onTabsChange}
        onClose={vi.fn()}
      />,
    );
    act(() =>
      useWorktreeEditorRevealStore.getState().request("w9", {
        path: "new.ts",
        line: 1,
        column: 1,
      }),
    );
    await waitFor(() => expect(onTabsChange.mock.lastCall?.[0]).toContain("new.ts"));
    const last = onTabsChange.mock.lastCall?.[0] as string[];
    expect(last).toContain("src/a.ts");
    expect(last).not.toContain("f0.ts");
  });

  it("a close prompt raised before the root arrived still discards the right tab after it does", async () => {
    const persist = vi.fn();
    const inset = (w: Workspace) => (
      <SourceEditorInset
        workspace={w}
        seedRelPath={null}
        initialTabs={["src/a.ts"]}
        restoreDrafts={{ "src/a.ts": draft }}
        onPersistDraft={persist}
        onClose={vi.fn()}
      />
    );
    const early = { ...remoteWs, location: { ...location, cwd: undefined } };
    const { rerender } = render(inset(early));
    fireEvent.click(screen.getByRole("button", { name: /^Close a\.ts/ }));
    expect(screen.getByTestId("worktree-discard-confirm")).toBeInTheDocument();
    rerender(inset(remoteWs));
    expect(await screen.findByTestId("editor-textarea")).toHaveValue("kept edit\n");
    fireEvent.click(screen.getByTestId("worktree-discard-confirm-yes"));
    await waitFor(() =>
      expect(screen.queryByTestId("worktree-tab-src/a.ts")).not.toBeInTheDocument(),
    );
    expect(persist).toHaveBeenLastCalledWith("src/a.ts", null);
  });

  it("a seed line given before the root arrived still lands once the file opens", async () => {
    const inset = (w: Workspace) => (
      <SourceEditorInset workspace={w} seedRelPath="src/a.ts" seedLine={7} onClose={vi.fn()} />
    );
    const { rerender } = render(inset({ ...remoteWs, location: { ...location, cwd: undefined } }));
    rerender(inset(remoteWs));
    await screen.findByTestId("editor-textarea");
    await waitFor(() => expect(editorHandle.revealLine).toHaveBeenCalledWith(7, 1));
  });

  it("a remote worktree at '/' is labelled '/' and opens files under it", async () => {
    render(
      <SourceEditorInset
        workspace={{ ...remoteWs, location: { ...location, cwd: "/" } }}
        seedRelPath="src/a.ts"
        onClose={vi.fn()}
      />,
    );
    expect(await screen.findByTestId("editor-textarea")).toHaveValue("remote text\n");
    expect(remote.read).toHaveBeenCalledWith("m-1", "h-1", "src/a.ts");
    expect(screen.getByTitle("/")).toHaveTextContent("/");
  });

  it("round 6: a root that moves under a dirty tab keeps the unsaved edits", async () => {
    const persist = vi.fn();
    const inset = (w: Workspace) => (
      <SourceEditorInset
        workspace={w}
        seedRelPath="src/a.ts"
        onPersistDraft={persist}
        onClose={vi.fn()}
      />
    );
    const { rerender } = render(inset(remoteWs));
    fireEvent.change(await screen.findByTestId("editor-textarea"), {
      target: { value: "edited\n" },
    });
    rerender(inset({ ...remoteWs, location: { ...location, cwd: "/home/dev/wt2" } }));
    await waitFor(() =>
      expect(screen.getByTestId("worktree-pane-active")).toHaveAttribute(
        "data-path",
        "/home/dev/wt2/src/a.ts",
      ),
    );
    expect(await screen.findByTestId("editor-textarea")).toHaveValue("edited\n");
    expect(screen.getByTestId("worktree-dirty-dot")).toBeInTheDocument();
    // The pane is re-targeted in place, not remounted: it still owns the draft,
    // so the only store call is the clean mount's (made before the edit).
    expect(persist).toHaveBeenCalledTimes(1);
    expect(persist).toHaveBeenCalledWith("src/a.ts", null);
  });
});

// A retained team's watch is released while it is hidden and the native attach emits no catch-up,
// so resume must reconcile through the editor's own disk-change path.
describe("WorktreeEditorSurface — resume after the team was hidden", () => {
  beforeEach(() => {
    useViewerStore.setState({
      worktreeEditorOpenBySession: { "sid-ws": true },
      worktreeEditorFile: { w1: "a.ts" },
      worktreeEditorTabs: {},
      worktreeEditorDraft: {},
    });
    act(() => warmScopes.activate("org/a"));
  });
  afterEach(() => {
    warmScopes.forget("org/a");
    warmScopes.forget("org/b");
  });

  const surface = () => (
    <WorktreeEditorSurface workspace={ws} teamKey="org/a" activeTabSessionId="sid-ws" isFocused />
  );

  async function hideTeam() {
    act(() => warmScopes.activate("org/b"));
    // The forwarded team key releases this editor's watch while the team is hidden.
    await waitFor(() => expect(detachWorktreeWatchMock).toHaveBeenCalledTimes(1));
  }

  it("a clean editor shows what changed on disk while its team was hidden", async () => {
    render(surface());
    expect(await screen.findByTestId("editor-textarea")).toHaveValue("const x = 1;\n");
    await hideTeam();
    readFileMock.mockResolvedValue("agent wrote this\n");
    act(() => warmScopes.activate("org/a"));
    await waitFor(() =>
      expect(screen.getByTestId("editor-textarea")).toHaveValue("agent wrote this\n"),
    );
    expect(lazyAttachWorktreeWatchMock).toHaveBeenCalledTimes(2);
  });

  it("takes editing focus again when its team is shown again", async () => {
    render(surface());
    await screen.findByTestId("editor-textarea");
    await hideTeam();
    editorHandle.focus.mockClear();
    act(() => warmScopes.activate("org/a"));
    expect(editorHandle.focus).toHaveBeenCalledTimes(1);
  });

  it("a dirty editor keeps both versions: the conflict guard arms, nothing is written", async () => {
    render(surface());
    fireEvent.change(await screen.findByTestId("editor-textarea"), {
      target: { value: "my local edit\n" },
    });
    await hideTeam();
    readFileMock.mockResolvedValue("agent wrote this\n");
    act(() => warmScopes.activate("org/a"));
    expect(await screen.findByTestId("worktree-stale-warning")).toBeInTheDocument();
    expect(screen.getByTestId("editor-textarea")).toHaveValue("my local edit\n");
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    expect(writeWorktreeFileMock).not.toHaveBeenCalled();
    // The disk side is still reachable through the existing merge view.
    fireEvent.click(screen.getByTestId("worktree-stale-resolve"));
    expect(await screen.findByTestId("mock-merge-view")).toHaveAttribute(
      "data-disk",
      "agent wrote this\n",
    );
  });

  it("an unchanged file is not reloaded into a different buffer on resume", async () => {
    render(surface());
    const textarea = await screen.findByTestId("editor-textarea");
    fireEvent.change(textarea, { target: { value: "my local edit\n" } });
    await hideTeam();
    act(() => warmScopes.activate("org/a"));
    await waitFor(() => expect(lazyAttachWorktreeWatchMock).toHaveBeenCalledTimes(2));
    expect(screen.getByTestId("editor-textarea")).toHaveValue("my local edit\n");
    expect(screen.queryByTestId("worktree-stale-warning")).not.toBeInTheDocument();
  });
});
