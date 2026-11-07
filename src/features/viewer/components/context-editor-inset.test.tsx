// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, act } from "@/test-utils";
import type React from "react";

import type { FileScope } from "@/features/dashboard/lib/file-scope";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import {
  MONO_FONT_STACK,
  PROSE_FONT_STACK,
} from "@/shared/components/code-editor/editor-affordances";

// SW-VE-9: controllable find-panel state for the mock CodeEditor. Tests toggle
// `open` to model "find is showing", read `closeCalls` to assert the inset
// closed it, and set `closeReturn` to model CM6 having already closed the panel
// between the capture-phase peek and the bubble-phase close (the AC-3 race).
const editorSearch = vi.hoisted(() => ({
  open: false,
  closeCalls: 0,
  closeReturn: null as boolean | null,
}));

// Stub the CM6 editor to a controlled textarea + a ⌘S trigger, and the editable
// merge view to a disk/local pair. The real value/onChange/onSave wiring is
// asserted here; CM internals live in the code-editor suites. SW-VE-9: the stub
// forwards a ref exposing `isSearchOpen`/`closeSearch` so the deterministic
// Escape path is exercisable without real CM6.
vi.mock("@/shared/components/code-editor", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/components/code-editor")>();
  const reactMod = await import("react");
  return {
    ...actual,
    CodeEditor: reactMod.forwardRef(function MockCodeEditor(
      {
        value,
        path,
        readOnly,
        gutters,
        lineWrap,
        search,
        folding,
        showWhitespace,
        tabSize,
        fontFamily,
        onChange,
        onSave,
      }: {
        value: string;
        path?: string;
        readOnly?: boolean;
        gutters?: boolean;
        lineWrap?: boolean;
        search?: boolean;
        folding?: boolean;
        showWhitespace?: boolean;
        tabSize?: number;
        fontFamily?: string;
        onChange?: (v: string) => void;
        onSave?: (v: string) => Promise<void>;
      },
      ref: React.ForwardedRef<{ isSearchOpen: () => boolean; closeSearch: () => boolean }>,
    ) {
      reactMod.useImperativeHandle(ref, () => ({
        isSearchOpen: () => editorSearch.open,
        closeSearch: () => {
          editorSearch.closeCalls += 1;
          const was = editorSearch.open;
          editorSearch.open = false;
          return editorSearch.closeReturn ?? was;
        },
      }));
      return (
        <div
          data-testid="mock-code-editor"
          data-path={path}
          data-readonly={String(readOnly)}
          data-gutters={String(gutters)}
          data-linewrap={String(lineWrap)}
          data-search={String(search)}
          data-folding={String(folding)}
          data-whitespace={String(showWhitespace)}
          data-tabsize={String(tabSize)}
          data-fontfamily={fontFamily}
        >
          <textarea
            data-testid="editor-textarea"
            value={value}
            onChange={(e) => onChange?.(e.target.value)}
          />
          <button type="button" data-testid="editor-cmd-s" onClick={() => void onSave?.(value)}>
            cmd-s
          </button>
        </div>
      );
    }),
    // SW-VE-4: the editable merge view. `data-disk` exposes the read-only disk
    // side for the live-refresh assertion; the right side is an uncontrolled
    // textarea (a re-render for a fresh diskValue does NOT reset typed edits — how
    // we prove "no remount"); ⌘S and getLocalValue() both read its current value.
    CodeMergeView: reactMod.forwardRef(function MockMergeView(
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
      const taRef = reactMod.useRef<HTMLTextAreaElement>(null);
      reactMod.useImperativeHandle(ref, () => ({
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
const writeContentFileMock = vi.fn<(p: string, c: string, expected?: string) => Promise<void>>();
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  readFile: (p: string) => readFileMock(p),
  writeContentFile: (p: string, c: string, expected?: string) =>
    writeContentFileMock(p, c, expected),
}));

const cacheGetMock = vi.fn<(p: string) => { content: string; refreshKey: number } | undefined>();
vi.mock("@/shared/lib/file-content-cache", () => ({
  fileContentCache: {
    get: (p: string) => cacheGetMock(p),
    set: vi.fn(),
  },
}));

const closeActiveFileTabMock = vi.fn<(p: string) => void>();
vi.mock("@/shared/lib/close-active-file-tab", () => ({
  closeActiveFileTab: (p: string) => closeActiveFileTabMock(p),
}));

import { ContextEditorInset, resetAgentOwnershipAdvisoryForTests } from "./context-editor-inset";

const PATH = "/ctx/team/config.yaml";
const OWN_SCOPE: FileScope = {
  repoSlug: null,
  sourceTeam: "acme",
  isInherited: false,
};

beforeEach(() => {
  resetAgentOwnershipAdvisoryForTests();
  readFileMock.mockReset();
  readFileMock.mockResolvedValue("key: v1\n");
  writeContentFileMock.mockReset();
  // A save lands on disk: later reads see what was written.
  writeContentFileMock.mockImplementation(async (_p, c) => {
    readFileMock.mockResolvedValue(c);
  });
  cacheGetMock.mockReset();
  cacheGetMock.mockReturnValue(undefined);
  closeActiveFileTabMock.mockReset();
  editorSearch.open = false;
  editorSearch.closeCalls = 0;
  editorSearch.closeReturn = null;
});

type InsetOverrides = {
  filePath?: string;
  scope?: FileScope | null;
  onExit?: () => void;
  onDirtyChange?: (dirty: boolean) => void;
  active?: boolean;
};

/** Render the inset with defaults + a `signalChange` that models an agent write:
 *  bump the content-repo refresh key (with the open path in the changed set) so
 *  the scoped nonce advances and the reconcile re-reads the OPEN file. */
function renderInset(overrides: InsetOverrides = {}) {
  const props = {
    filePath: PATH,
    scope: OWN_SCOPE as FileScope | null,
    onExit: vi.fn(),
    ...overrides,
  };
  let refreshKey = 0;
  let active = overrides.active ?? true;
  const utils = render(
    <ContextEditorInset {...props} refreshKey={refreshKey} changedPaths={null} active={active} />,
  );
  const rerender = (changed: Set<string> | null) =>
    utils.rerender(
      <ContextEditorInset
        {...props}
        refreshKey={refreshKey}
        changedPaths={changed}
        active={active}
      />,
    );
  const signalChange = (changed: Set<string> | null = new Set([props.filePath])) => {
    refreshKey += 1;
    act(() => rerender(changed));
  };
  // Toggle the sync-review surface (active=false → the parent display:none's the
  // inset). Used to prove a mid-reconcile hand-merge survives the round-trip.
  const setActive = (next: boolean) => {
    active = next;
    act(() => rerender(null));
  };
  return { ...utils, props, signalChange, setActive };
}

/** Mount + wait for the editable editor (async read path). */
async function openEditor(scope: FileScope | null = OWN_SCOPE) {
  const utils = renderInset({ scope });
  const textarea = await screen.findByTestId("editor-textarea");
  return { ...utils, textarea };
}

describe("SW-VE-11 — editor power-config affordances passed to <CodeEditor>", () => {
  beforeEach(() => {
    // Reset to shipped defaults so each case controls exactly what it tests.
    useViewerStore.setState({
      settings: {
        ...useViewerStore.getState().settings,
        editorFolding: "auto",
        editorShowWhitespace: false,
        editorTabSize: 2,
        editorProseFont: false,
      },
    });
  });

  it("defaults: code file folds (auto→code), no whitespace, tab size 2, mono font", async () => {
    readFileMock.mockResolvedValueOnce("key: v\n");
    renderInset({ filePath: "/ctx/team/config.yaml" });
    const ed = await screen.findByTestId("mock-code-editor");
    expect(ed).toHaveAttribute("data-folding", "true");
    expect(ed).toHaveAttribute("data-whitespace", "false");
    expect(ed).toHaveAttribute("data-tabsize", "2");
    expect(ed).toHaveAttribute("data-fontfamily", MONO_FONT_STACK);
  });

  it("markdown does NOT fold (auto→prose off) and stays mono when prose-font is off", async () => {
    readFileMock.mockResolvedValueOnce("# Title\n");
    renderInset({ filePath: "/ctx/team/README.md" });
    const ed = await screen.findByTestId("mock-code-editor");
    expect(ed).toHaveAttribute("data-folding", "false");
    expect(ed).toHaveAttribute("data-fontfamily", MONO_FONT_STACK);
  });

  it("prose-font on → markdown gets the reading stack; whitespace + tab size flow through", async () => {
    useViewerStore.setState({
      settings: {
        ...useViewerStore.getState().settings,
        editorProseFont: true,
        editorShowWhitespace: true,
        editorTabSize: 8,
      },
    });
    readFileMock.mockResolvedValueOnce("# Title\n");
    renderInset({ filePath: "/ctx/team/README.md" });
    const ed = await screen.findByTestId("mock-code-editor");
    expect(ed).toHaveAttribute("data-fontfamily", PROSE_FONT_STACK);
    expect(ed).toHaveAttribute("data-whitespace", "true");
    expect(ed).toHaveAttribute("data-tabsize", "8");
  });

  it("prose-font on → CODE file still mono (code never leaves monospace)", async () => {
    useViewerStore.setState({
      settings: { ...useViewerStore.getState().settings, editorProseFont: true },
    });
    readFileMock.mockResolvedValueOnce("key: v\n");
    renderInset({ filePath: "/ctx/team/config.yaml" });
    const ed = await screen.findByTestId("mock-code-editor");
    expect(ed).toHaveAttribute("data-fontfamily", MONO_FONT_STACK);
  });
});

describe("SW-VE-8 — prose-grade editor config passed to <CodeEditor> (AC-10/12/14)", () => {
  it("code file (yaml): gutter ON, no wrap, find enabled", async () => {
    readFileMock.mockResolvedValueOnce("key: v\n");
    renderInset({ filePath: "/ctx/team/config.yaml" });
    const ed = await screen.findByTestId("mock-code-editor");
    expect(ed).toHaveAttribute("data-gutters", "true");
    expect(ed).toHaveAttribute("data-linewrap", "false");
    expect(ed).toHaveAttribute("data-search", "true");
  });

  it("markdown file: gutter OFF, wraps (auto → prose), find enabled", async () => {
    readFileMock.mockResolvedValueOnce("# Title\n\nbody\n");
    renderInset({ filePath: "/ctx/team/README.md" });
    const ed = await screen.findByTestId("mock-code-editor");
    expect(ed).toHaveAttribute("data-gutters", "false");
    expect(ed).toHaveAttribute("data-linewrap", "true");
    expect(ed).toHaveAttribute("data-search", "true");
  });

  it("html file edits code-side (gutter ON, no wrap)", async () => {
    readFileMock.mockResolvedValueOnce("<div>hi</div>\n");
    renderInset({ filePath: "/ctx/team/dashboard.html" });
    const ed = await screen.findByTestId("mock-code-editor");
    expect(ed).toHaveAttribute("data-gutters", "true");
    expect(ed).toHaveAttribute("data-linewrap", "false");
  });
});

describe("ContextEditorInset — read/gate (AC-1)", () => {
  it("mounts an EDITABLE editor with the file's content (read-only default is off here)", async () => {
    readFileMock.mockResolvedValueOnce("key: hello\n");
    const { textarea } = await openEditor();
    expect(textarea).toHaveValue("key: hello\n");
    expect(screen.getByTestId("mock-code-editor")).not.toHaveAttribute("data-readonly", "true");
    expect(screen.getByTestId("mock-code-editor")).toHaveAttribute("data-path", PATH);
  });

  it("seeds instantly from the content cache (no skeleton flash on entry)", () => {
    cacheGetMock.mockReturnValue({ content: "cached: yes\n", refreshKey: 0 });
    renderInset();
    // Editable immediately — no async wait, no skeleton.
    expect(screen.getByTestId("editor-textarea")).toHaveValue("cached: yes\n");
    expect(screen.queryByTestId("context-editor-skeleton")).not.toBeInTheDocument();
  });

  it("§B: binary content shows a placeholder, not an editor", async () => {
    readFileMock.mockResolvedValueOnce("\0binary");
    renderInset();
    expect(await screen.findByTestId("context-editor-placeholder")).toBeInTheDocument();
    expect(screen.queryByTestId("mock-code-editor")).not.toBeInTheDocument();
  });

  it("a read failure shows an inline error + Retry that re-reads", async () => {
    readFileMock.mockRejectedValueOnce(new Error("EACCES"));
    renderInset();
    expect(await screen.findByTestId("context-editor-read-error")).toBeInTheDocument();
    readFileMock.mockResolvedValueOnce("recovered: 1\n");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByTestId("editor-textarea")).toHaveValue("recovered: 1\n");
  });

  it("shows the 'Editing in <scope>' chip for an own-writable scope", async () => {
    await openEditor();
    expect(screen.getByTestId("editing-in-scope")).toBeInTheDocument();
  });

  it("active=false makes the editor readOnly (suppresses blur-save on the review surface)", async () => {
    renderInset({ active: false });
    await screen.findByTestId("editor-textarea");
    expect(screen.getByTestId("mock-code-editor")).toHaveAttribute("data-readonly", "true");
  });
});

describe("ContextEditorInset — dirty + save (AC-5)", () => {
  it("a private save immediately refreshes its own content source", async () => {
    const root = "/home/users/42/scratch/orgs/acme/plans/content";
    const path = `${root}/2026/plan.md`;
    useFileRefreshStore.setState({
      fileRefreshKeys: {},
      contentRefreshKeys: {},
      lastChangedContextFile: {},
    });
    renderInset({ filePath: path });
    fireEvent.change(await screen.findByTestId("editor-textarea"), {
      target: { value: "Private edit" },
    });
    fireEvent.click(screen.getByTestId("context-editor-save"));
    await waitFor(() => expect(useFileRefreshStore.getState().contentRefreshKeys[root]).toBe(1));
    expect(writeContentFileMock).toHaveBeenCalledWith(path, "Private edit", expect.any(String));
  });
  it("shows a dirty dot when the buffer diverges and clears it after a successful save", async () => {
    const { textarea } = await openEditor();
    expect(screen.queryByTestId("context-editor-dirty-dot")).not.toBeInTheDocument();

    fireEvent.change(textarea, { target: { value: "key: v2\n" } });
    expect(screen.getByTestId("context-editor-dirty-dot")).toBeInTheDocument();

    fireEvent.click(screen.getByTestId("context-editor-save"));
    await waitFor(() =>
      expect(writeContentFileMock).toHaveBeenCalledWith(PATH, "key: v2\n", expect.any(String)),
    );
    await waitFor(() =>
      expect(screen.queryByTestId("context-editor-dirty-dot")).not.toBeInTheDocument(),
    );
  });

  it("a clean buffer never fires a write", async () => {
    await openEditor();
    fireEvent.click(screen.getByTestId("context-editor-save"));
    await Promise.resolve();
    expect(writeContentFileMock).not.toHaveBeenCalled();
  });

  it("⌘S from the editor saves the current buffer", async () => {
    const { textarea } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: edited\n" } });
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    await waitFor(() =>
      expect(writeContentFileMock).toHaveBeenCalledWith(PATH, "key: edited\n", expect.any(String)),
    );
  });

  it("trailing-flushes a keystroke typed during an in-flight save", async () => {
    const { textarea } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: v2\n" } });

    // Hold the first write open; type again while it's in flight, then release.
    let releaseWrite: () => void = () => {};
    writeContentFileMock.mockImplementationOnce(
      () => new Promise<void>((res) => (releaseWrite = () => res())),
    );
    fireEvent.click(screen.getByTestId("context-editor-save"));
    await waitFor(() =>
      expect(writeContentFileMock).toHaveBeenCalledWith(PATH, "key: v2\n", expect.any(String)),
    );

    // A keystroke lands during the in-flight write, then the write resolves.
    fireEvent.change(textarea, { target: { value: "key: v3\n" } });
    releaseWrite();

    // The trailing flush persists the LATEST content without a second ⌘S.
    await waitFor(() =>
      expect(writeContentFileMock).toHaveBeenCalledWith(PATH, "key: v3\n", expect.any(String)),
    );
  });

  it("lifts the dirty flag via onDirtyChange", async () => {
    const onDirtyChange = vi.fn();
    renderInset({ onDirtyChange });
    const textarea = await screen.findByTestId("editor-textarea");
    onDirtyChange.mockClear();
    fireEvent.change(textarea, { target: { value: "key: dirtynow\n" } });
    await waitFor(() => expect(onDirtyChange).toHaveBeenLastCalledWith(true));
  });
});

describe("ContextEditorInset — exit safety (AC-5)", () => {
  it("exiting while dirty shows an inline discard-confirm; Discard exits, Keep stays", async () => {
    const onExit = vi.fn();
    const { textarea } = await (async () => {
      const utils = renderInset({ onExit });
      const ta = await screen.findByTestId("editor-textarea");
      return { ...utils, textarea: ta };
    })();
    fireEvent.change(textarea, { target: { value: "key: v2\n" } });

    fireEvent.click(screen.getByTestId("context-editor-done"));
    expect(screen.getByTestId("context-editor-discard-confirm")).toBeInTheDocument();
    expect(onExit).not.toHaveBeenCalled();

    fireEvent.click(screen.getByTestId("context-editor-discard-confirm-no"));
    expect(screen.queryByTestId("context-editor-discard-confirm")).not.toBeInTheDocument();
    expect(onExit).not.toHaveBeenCalled();

    fireEvent.click(screen.getByTestId("context-editor-done"));
    fireEvent.click(screen.getByTestId("context-editor-discard-confirm-yes"));
    expect(onExit).toHaveBeenCalledTimes(1);
  });

  it("exiting a clean buffer leaves immediately (no confirm)", async () => {
    const onExit = vi.fn();
    renderInset({ onExit });
    await screen.findByTestId("editor-textarea");
    fireEvent.click(screen.getByTestId("context-editor-done"));
    expect(screen.queryByTestId("context-editor-discard-confirm")).not.toBeInTheDocument();
    expect(onExit).toHaveBeenCalledTimes(1);
  });

  it("a save error keeps the buffer dirty with an inline Retry (edits never dropped)", async () => {
    const { textarea } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: v2\n" } });
    writeContentFileMock.mockRejectedValueOnce(new Error("EPERM"));
    fireEvent.click(screen.getByTestId("context-editor-save"));

    expect(await screen.findByTestId("context-editor-save-error")).toBeInTheDocument();
    // still dirty
    expect(screen.getByTestId("context-editor-dirty-dot")).toBeInTheDocument();

    writeContentFileMock.mockResolvedValueOnce(undefined);
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() =>
      expect(writeContentFileMock).toHaveBeenLastCalledWith(PATH, "key: v2\n", expect.any(String)),
    );
  });
});

describe("SW-VE-9 — deterministic find Escape (AC-2/3/4)", () => {
  it("first Escape with find open closes find (no exit); second Escape exits", async () => {
    const onExit = vi.fn();
    renderInset({ onExit });
    await screen.findByTestId("editor-textarea");
    const inset = screen.getByTestId("context-editor-inset");

    // Find is open. First Escape must close find and keep the edit session.
    editorSearch.open = true;
    fireEvent.keyDown(inset, { key: "Escape" });
    expect(editorSearch.closeCalls).toBe(1);
    expect(onExit).not.toHaveBeenCalled();

    // Find is now closed (buffer is clean). Second Escape exits.
    fireEvent.keyDown(inset, { key: "Escape" });
    expect(onExit).toHaveBeenCalledTimes(1);
  });

  it("does NOT exit even when CM6 already closed the panel before bubble (AC-3 race)", async () => {
    // isSearchOpen() is true at the capture-phase peek, but closeSearch()
    // reports false (CM6's own searchKeymap closed it first). The decision must
    // hinge on the CAPTURED flag, not on closeSearch()'s return.
    const onExit = vi.fn();
    renderInset({ onExit });
    await screen.findByTestId("editor-textarea");
    const inset = screen.getByTestId("context-editor-inset");

    editorSearch.open = true;
    editorSearch.closeReturn = false;
    fireEvent.keyDown(inset, { key: "Escape" });
    // The captured flag drives a real close attempt (not a no-op) AND blocks exit.
    expect(editorSearch.closeCalls).toBe(1);
    expect(onExit).not.toHaveBeenCalled();
  });

  it("closes find regardless of the Escape target/focus (fires from the editor body)", async () => {
    // Focus-independence is the whole point of replacing the old
    // `.closest(".cm-panels")` heuristic: the capture-phase peek fires no matter
    // which descendant the Escape originates from. Dispatch from the textarea
    // (editor body) rather than the inset root.
    const onExit = vi.fn();
    renderInset({ onExit });
    const textarea = await screen.findByTestId("editor-textarea");

    editorSearch.open = true;
    fireEvent.keyDown(textarea, { key: "Escape" });
    expect(editorSearch.closeCalls).toBe(1);
    expect(onExit).not.toHaveBeenCalled();
  });

  it("active=false (review surface): find Escape still closes find, not the session", async () => {
    // Under the sync-review surface the inset is readOnly; CM6 find still works,
    // so the deterministic Escape must behave identically there.
    const onExit = vi.fn();
    renderInset({ onExit, active: false });
    await screen.findByTestId("editor-textarea");
    const inset = screen.getByTestId("context-editor-inset");

    editorSearch.open = true;
    fireEvent.keyDown(inset, { key: "Escape" });
    expect(editorSearch.closeCalls).toBe(1);
    expect(onExit).not.toHaveBeenCalled();
  });

  it("Escape with find closed exits immediately (clean buffer) — no find interference", async () => {
    const onExit = vi.fn();
    renderInset({ onExit });
    await screen.findByTestId("editor-textarea");
    const inset = screen.getByTestId("context-editor-inset");

    editorSearch.open = false;
    fireEvent.keyDown(inset, { key: "Escape" });
    expect(editorSearch.closeCalls).toBe(0);
    expect(onExit).toHaveBeenCalledTimes(1);
  });

  it("pendingDiscard owns Escape before find (AC-4) — cancels the confirm, leaves find alone", async () => {
    const onExit = vi.fn();
    renderInset({ onExit });
    const textarea = await screen.findByTestId("editor-textarea");
    fireEvent.change(textarea, { target: { value: "key: dirty\n" } });

    // Arm the discard confirm (dirty exit), then press Escape with find "open".
    fireEvent.click(screen.getByTestId("context-editor-done"));
    expect(screen.getByTestId("context-editor-discard-confirm")).toBeInTheDocument();

    editorSearch.open = true;
    const inset = screen.getByTestId("context-editor-inset");
    fireEvent.keyDown(inset, { key: "Escape" });

    // The confirm is cancelled; find is untouched and edit is not torn down.
    expect(screen.queryByTestId("context-editor-discard-confirm")).not.toBeInTheDocument();
    expect(editorSearch.closeCalls).toBe(0);
    expect(onExit).not.toHaveBeenCalled();
  });
});

describe("ContextEditorInset — concurrent-write safety (SW-VE-4)", () => {
  it.each([PATH, "/ctx/team"])(
    "AC-2: a clean buffer adopts a file or ancestor change at %s",
    async (changed) => {
      const { textarea, signalChange } = await openEditor();
      expect(textarea).toHaveValue("key: v1\n");

      // Agent overwrites the open .yaml; the content-repo signal implicates it.
      readFileMock.mockResolvedValue("key: agent\n");
      signalChange(new Set([changed]));

      await waitFor(() =>
        expect(screen.getByTestId("editor-textarea")).toHaveValue("key: agent\n"),
      );
      // Silent — no stale banner, no write.
      expect(screen.queryByTestId("context-editor-stale-warning")).not.toBeInTheDocument();
      expect(writeContentFileMock).not.toHaveBeenCalled();
    },
  );

  it("AC-1: a dirty buffer + agent write warns + BLOCKS the clobbering save (no write)", async () => {
    const { textarea, signalChange } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: mine\n" } });

    readFileMock.mockResolvedValue("key: agent\n");
    signalChange();
    expect(await screen.findByTestId("context-editor-stale-warning")).toBeInTheDocument();

    // ⌘S / Save must NOT clobber the agent's write while the guard is armed.
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    fireEvent.click(screen.getByTestId("context-editor-save"));
    expect(writeContentFileMock).not.toHaveBeenCalled();
    expect(screen.getByTestId("editor-textarea")).toHaveValue("key: mine\n");
  });

  it("AC-1: an unrelated file's write (open file unchanged) is a no-op — no warn/reload", async () => {
    const { textarea, signalChange } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: editing\n" } });
    // The open file's disk is unchanged; the burst names a DIFFERENT file.
    readFileMock.mockResolvedValue("key: v1\n");
    signalChange(new Set(["/ctx/team/other.yaml"]));

    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.queryByTestId("context-editor-stale-warning")).not.toBeInTheDocument();
    expect(screen.getByTestId("editor-textarea")).toHaveValue("key: editing\n");
    // The open file wasn't even re-read (per-path scoping).
    expect(readFileMock).toHaveBeenCalledTimes(1);
  });

  it("AC-1: the editor's OWN save echo reconciles to a no-op (no false warning)", async () => {
    const { textarea, signalChange } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: saved\n" } });
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    await waitFor(() =>
      expect(writeContentFileMock).toHaveBeenCalledWith(PATH, "key: saved\n", expect.any(String)),
    );

    // The watcher echo of our own write arrives (disk === lastSaved) while the
    // user types again — no false "disk changed" warning.
    readFileMock.mockResolvedValue("key: saved\n");
    fireEvent.change(screen.getByTestId("editor-textarea"), {
      target: { value: "key: saved + more\n" },
    });
    signalChange();
    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.queryByTestId("context-editor-stale-warning")).not.toBeInTheDocument();
  });

  it("AC-3: the stale banner's Resolve opens the disk-vs-buffer merge view (fresh disk vs buffer, no write)", async () => {
    const { textarea, signalChange } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: mine\n" } });
    readFileMock.mockResolvedValue("key: agent\n");
    signalChange();
    await screen.findByTestId("context-editor-stale-warning");

    fireEvent.click(screen.getByTestId("context-editor-stale-resolve"));
    const merge = await screen.findByTestId("mock-merge-view");
    expect(merge).toHaveAttribute("data-disk", "key: agent\n");
    expect(screen.getByTestId("merge-local")).toHaveValue("key: mine\n");
    expect(screen.queryByTestId("mock-code-editor")).not.toBeInTheDocument();
    expect(writeContentFileMock).not.toHaveBeenCalled();
    // Banner hidden while the merge view is up.
    expect(screen.queryByTestId("context-editor-stale-warning")).not.toBeInTheDocument();
  });

  it("saves are locked: the writer gets the SHA-256 of the text the buffer is based on", async () => {
    const { textarea } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: mine\n" } });
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    await waitFor(() => expect(writeContentFileMock).toHaveBeenCalled());
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode("key: v1\n"));
    const hex = Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
    expect(writeContentFileMock).toHaveBeenCalledWith(PATH, "key: mine\n", hex);
  });

  it("a save the writer refuses (disk moved) shows the conflict, never overwriting", async () => {
    const { textarea } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: mine\n" } });
    writeContentFileMock.mockImplementationOnce(async () => {
      readFileMock.mockResolvedValue("key: agent\n");
      throw `conflict: ${PATH} changed on disk since it was opened`;
    });
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    expect(await screen.findByTestId("context-editor-stale-warning")).toBeInTheDocument();
    expect(screen.getByTestId("editor-textarea")).toHaveValue("key: mine\n");
  });

  it("a change mid-merge offers Refresh, which updates the disk side", async () => {
    const { textarea, signalChange } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: mine\n" } });
    readFileMock.mockResolvedValue("key: agent\n");
    signalChange();
    await screen.findByTestId("context-editor-stale-warning");
    fireEvent.click(screen.getByTestId("context-editor-stale-resolve"));
    await screen.findByTestId("mock-merge-view");
    readFileMock.mockResolvedValue("key: agent v2\n");
    signalChange();
    fireEvent.click(await screen.findByTestId("context-editor-reconcile-refresh"));
    await waitFor(() =>
      expect(screen.getByTestId("mock-merge-view")).toHaveAttribute("data-disk", "key: agent v2\n"),
    );
  });

  it("AC-3: Take theirs adopts the reviewed disk (no write), clears the conflict", async () => {
    const { textarea, signalChange } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: mine\n" } });
    readFileMock.mockResolvedValue("key: agent\n");
    signalChange();
    await screen.findByTestId("context-editor-stale-warning");
    fireEvent.click(screen.getByTestId("context-editor-stale-resolve"));
    await screen.findByTestId("mock-merge-view");

    fireEvent.click(screen.getByTestId("context-editor-reconcile-take-theirs"));
    await waitFor(() => expect(screen.getByTestId("editor-textarea")).toHaveValue("key: agent\n"));
    expect(screen.queryByTestId("mock-merge-view")).not.toBeInTheDocument();
    expect(screen.queryByTestId("context-editor-dirty-dot")).not.toBeInTheDocument();
    expect(writeContentFileMock).not.toHaveBeenCalled();
  });

  it("AC-3: Keep mine writes the buffer — the ONE sanctioned clobber (bypasses the guard)", async () => {
    const { textarea, signalChange } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: mine\n" } });
    readFileMock.mockResolvedValue("key: agent\n");
    signalChange();
    await screen.findByTestId("context-editor-stale-warning");
    fireEvent.click(screen.getByTestId("context-editor-stale-resolve"));
    await screen.findByTestId("mock-merge-view");

    fireEvent.click(screen.getByTestId("context-editor-reconcile-keep-mine"));
    await waitFor(() =>
      expect(writeContentFileMock).toHaveBeenCalledWith(PATH, "key: mine\n", expect.any(String)),
    );
    await waitFor(() => expect(screen.queryByTestId("mock-merge-view")).not.toBeInTheDocument());
    expect(screen.queryByTestId("context-editor-dirty-dot")).not.toBeInTheDocument();
  });

  it("AC-3: hand-merge — edit the merge view and ⌘S writes the resolved result", async () => {
    const { textarea, signalChange } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: mine\n" } });
    readFileMock.mockResolvedValue("key: agent\n");
    signalChange();
    await screen.findByTestId("context-editor-stale-warning");
    fireEvent.click(screen.getByTestId("context-editor-stale-resolve"));
    await screen.findByTestId("mock-merge-view");

    fireEvent.change(screen.getByTestId("merge-local"), { target: { value: "key: merged\n" } });
    fireEvent.click(screen.getByTestId("merge-cmd-s"));
    await waitFor(() =>
      expect(writeContentFileMock).toHaveBeenCalledWith(PATH, "key: merged\n", expect.any(String)),
    );
    await waitFor(() => expect(screen.getByTestId("editor-textarea")).toHaveValue("key: merged\n"));
  });

  it("AC-3: Cancel returns to the still-armed guard — a plain ⌘S is still blocked", async () => {
    const { textarea, signalChange } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: mine\n" } });
    readFileMock.mockResolvedValue("key: agent\n");
    signalChange();
    await screen.findByTestId("context-editor-stale-warning");
    fireEvent.click(screen.getByTestId("context-editor-stale-resolve"));
    await screen.findByTestId("mock-merge-view");

    fireEvent.click(screen.getByTestId("context-editor-reconcile-cancel"));
    expect(await screen.findByTestId("context-editor-stale-warning")).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("editor-cmd-s"));
    expect(writeContentFileMock).not.toHaveBeenCalled();
  });

  it("AC-3: Reload takes the disk version and clears the conflict (no write)", async () => {
    const { textarea, signalChange } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: mine\n" } });
    readFileMock.mockResolvedValue("key: agent\n");
    signalChange();
    await screen.findByTestId("context-editor-stale-warning");

    fireEvent.click(screen.getByTestId("context-editor-stale-reload"));
    await waitFor(() => expect(screen.getByTestId("editor-textarea")).toHaveValue("key: agent\n"));
    expect(screen.queryByTestId("context-editor-stale-warning")).not.toBeInTheDocument();
    expect(screen.queryByTestId("context-editor-dirty-dot")).not.toBeInTheDocument();
    expect(writeContentFileMock).not.toHaveBeenCalled();
  });

  it("AC-4: an agent deleting the open file (clean buffer) self-heals to a 'was removed' state", async () => {
    const { signalChange } = await openEditor();
    // Clean buffer; the agent removes the file underneath.
    readFileMock.mockRejectedValue(new Error("No such file or directory (os error 2)"));
    signalChange();

    expect(await screen.findByText("This file was removed from disk")).toBeInTheDocument();
    expect(screen.queryByTestId("mock-code-editor")).not.toBeInTheDocument();
  });

  it("AC-4: a DIRTY buffer is NOT vanished on delete — edits are kept (a save recreates it)", async () => {
    const { textarea, signalChange } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: precious\n" } });
    readFileMock.mockRejectedValue(new Error("No such file or directory (os error 2)"));
    signalChange();

    await act(async () => {
      await Promise.resolve();
    });
    // Edits survive; no "removed" empty state stole them.
    expect(screen.queryByText("This file was removed from disk")).not.toBeInTheDocument();
    expect(screen.getByTestId("editor-textarea")).toHaveValue("key: precious\n");
  });

  it("AC-4: a re-created file recovers from the 'was removed' state on the next change", async () => {
    const { signalChange } = await openEditor();
    readFileMock.mockRejectedValue(new Error("No such file or directory (os error 2)"));
    signalChange();
    expect(await screen.findByText("This file was removed from disk")).toBeInTheDocument();

    // The agent re-creates the file; the next change bump restores the editor
    // (no stuck placeholder).
    readFileMock.mockResolvedValue("key: reborn\n");
    signalChange();
    await waitFor(() => expect(screen.getByTestId("editor-textarea")).toHaveValue("key: reborn\n"));
    expect(screen.queryByText("This file was removed from disk")).not.toBeInTheDocument();
  });

  it("code-review M2: a mid-reconcile hand-merge survives the sync-review round-trip (active toggle)", async () => {
    const { textarea, signalChange, setActive } = await openEditor();
    fireEvent.change(textarea, { target: { value: "key: mine\n" } });
    readFileMock.mockResolvedValue("key: agent\n");
    signalChange();
    await screen.findByTestId("context-editor-stale-warning");
    fireEvent.click(screen.getByTestId("context-editor-stale-resolve"));
    await screen.findByTestId("mock-merge-view");
    // User types a hand-merge into the (uncontrolled, CM6-local) right side.
    fireEvent.change(screen.getByTestId("merge-local"), { target: { value: "key: merged\n" } });

    // The git-sync review surface turns on (active=false) and back off. The merge
    // view must NOT unmount (that would drop the typed hand-merge).
    setActive(false);
    setActive(true);

    expect(screen.getByTestId("mock-merge-view")).toBeInTheDocument();
    expect(screen.getByTestId("merge-local")).toHaveValue("key: merged\n");
  });
});

describe("ContextEditorInset — agent-ownership advisory (AC-6)", () => {
  it("shows the cautionary advisory once per file per session, dismissible", async () => {
    const { unmount } = renderInset();
    await screen.findByTestId("editor-textarea");
    expect(screen.getByTestId("context-editor-advisory")).toBeInTheDocument();

    fireEvent.click(screen.getByTestId("context-editor-advisory-dismiss"));
    expect(screen.queryByTestId("context-editor-advisory")).not.toBeInTheDocument();
    unmount();

    // Re-entering edit on the SAME file this session does not re-nag.
    renderInset();
    await screen.findByTestId("editor-textarea");
    expect(screen.queryByTestId("context-editor-advisory")).not.toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// SW-VE-10 — rendered live-preview pane (markdown-only, opt-in, active-gated)
// ---------------------------------------------------------------------------
describe("SW-VE-10 — live-preview pane", () => {
  it("markdown file: shows the preview toggle; toggling mounts a rendered pane", async () => {
    readFileMock.mockResolvedValueOnce("# Hello\n\nworld\n");
    renderInset({ filePath: "/ctx/team/README.md" });
    await screen.findByTestId("editor-textarea");

    const toggle = screen.getByTestId("context-editor-preview-toggle");
    // Default off — no pane yet.
    expect(screen.queryByTestId("markdown-preview-pane")).not.toBeInTheDocument();

    fireEvent.click(toggle);
    await screen.findByTestId("markdown-preview-pane");
    // Renders the live buffer as prose (deferred value settles to current buffer).
    const heading = await screen.findByRole("heading", { name: "Hello" });
    expect(heading.tagName).toBe("H1");

    // Toggling off unmounts the pane (no preview cost when closed).
    fireEvent.click(toggle);
    expect(screen.queryByTestId("markdown-preview-pane")).not.toBeInTheDocument();
  });

  it("non-markdown file (yaml): no preview toggle at all", async () => {
    readFileMock.mockResolvedValueOnce("key: v\n");
    renderInset({ filePath: "/ctx/team/config.yaml" });
    await screen.findByTestId("editor-textarea");
    expect(screen.queryByTestId("context-editor-preview-toggle")).not.toBeInTheDocument();
  });

  it("sync-hidden inset (active=false): preview stays unmounted even when toggled on", async () => {
    readFileMock.mockResolvedValueOnce("# Doc\n");
    const { setActive } = renderInset({ filePath: "/ctx/team/README.md" });
    await screen.findByTestId("editor-textarea");

    fireEvent.click(screen.getByTestId("context-editor-preview-toggle"));
    expect(screen.getByTestId("markdown-preview-pane")).toBeInTheDocument();

    // The parent turns on the git-sync review surface → active=false. The pane
    // must do NO render work while hidden (R-5 sync purity + perf).
    setActive(false);
    expect(screen.queryByTestId("markdown-preview-pane")).not.toBeInTheDocument();

    // Restoring active brings the (still-toggled-on) preview back.
    setActive(true);
    expect(screen.getByTestId("markdown-preview-pane")).toBeInTheDocument();
  });
});
