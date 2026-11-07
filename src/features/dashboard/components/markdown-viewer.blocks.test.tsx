// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { StrictMode } from "react";
import { render, waitFor, act, fireEvent } from "@testing-library/react";
import type { Block } from "@/shared/lib/markdown-worker/types";

// ---------------------------------------------------------------------------
// VIEWER_PERF mock: blocks mode enabled. Overrides the global mock in
// src/test-setup.ts for this file only (vi.mock hoists, applies per-file).
// ---------------------------------------------------------------------------

vi.mock("@/shared/lib/viewer-perf-config", () => ({
  VIEWER_PERF: {
    fileCacheMaxEntries: 25,
    keepAliveMaxViewers: 10,
    parseInWorker: true,
    renderMode: "blocks" as const,
  },
}));

// ---------------------------------------------------------------------------
// parseToBlocks mock — resolve with a deterministic Block[] so we can
// assert the viewer's rendering behavior without running a real worker.
// ---------------------------------------------------------------------------

const resolveMocks: {
  resolve?: (r: {
    blocks: Block[];
    perf: { parseMs: number; runMs: number; splitMs: number; postAt: number };
    receiveAt: number;
  }) => void;
} = {};

vi.mock("@/shared/lib/markdown-worker/client", () => ({
  parseMarkdown: vi.fn(() => ({
    promise: new Promise(() => {}),
    cancel: vi.fn(),
  })),
  parseToBlocks: vi.fn(() => {
    const promise = new Promise<{
      blocks: Block[];
      perf: {
        parseMs: number;
        runMs: number;
        splitMs: number;
        postAt: number;
      };
      receiveAt: number;
    }>((resolve) => {
      resolveMocks.resolve = resolve;
    });
    return { promise, cancel: vi.fn() };
  }),
}));

// ---------------------------------------------------------------------------
// Standard viewer mocks (same shape as markdown-viewer.coverage.test.tsx)
// ---------------------------------------------------------------------------

vi.mock("@/features/dashboard/components/html-viewer", () => ({ HtmlViewer: () => null }));
vi.mock("@/features/dashboard/components/excalidraw-viewer", () => ({
  ExcalidrawViewer: () => null,
}));
vi.mock("@/features/dashboard/components/image-viewer", () => ({ ImageViewer: () => null }));
vi.mock("@/features/viewer/components/code-file-viewer", () => ({ CodeFileViewer: () => null }));
vi.mock("@/features/git-sync/components/markdown-redline-viewer", () => ({
  MarkdownRedlineViewer: () => null,
}));
vi.mock("@/features/git-sync/components/before-after-diff-toggle", () => ({
  BeforeAfterDiffToggle: () => null,
}));
const mockReadFile = vi.fn<(path: string) => Promise<string>>();
const mockQueryContext = vi.fn();
vi.mock("@/tauri/commands", () => ({
  readFile: (...args: unknown[]) => mockReadFile(...(args as [string])),
  queryContext: (...args: unknown[]) => mockQueryContext(...args),
}));

const mockStoreValues: Record<string, unknown> = {
  contentWidth: "normal",
  appTheme: "dracula",
  viewerFontSize: "normal",
  selectedRepoPath: "/repo",
};
const mockCloseTab = vi.fn();

vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: Object.assign(
    (selector: (s: Record<string, unknown>) => unknown) =>
      selector({
        settings: {
          contentWidth: mockStoreValues.contentWidth,
          appTheme: mockStoreValues.appTheme,
          viewerFontSize: mockStoreValues.viewerFontSize,
        },
        selectedRepoPath: mockStoreValues.selectedRepoPath,
      }),
    {
      subscribe: () => () => {},
      getState: () => ({
        selectedRepoPath: mockStoreValues.selectedRepoPath,
        closeTab: mockCloseTab,
        setOpenFileLifecycle: vi.fn(),
      }),
    },
  ),
}));

vi.mock("next-themes", () => ({
  useTheme: () => ({ resolvedTheme: "dark" }),
}));

const mockMermaidRender = vi.fn<(id: string, code: string) => Promise<{ svg: string }>>();
vi.mock("mermaid", () => ({
  default: {
    initialize: vi.fn(),
    render: (...args: unknown[]) => mockMermaidRender(...(args as [string, string])),
  },
}));

// Import AFTER mocks so the VIEWER_PERF override applies.
import { MarkdownViewer } from "./markdown-viewer";
import { fileContentCache } from "@/shared/lib/file-content-cache";
import { parseResultCache } from "@/shared/lib/parse-result-cache";

beforeEach(() => {
  vi.clearAllMocks();
  mockReadFile.mockResolvedValue("irrelevant — parseToBlocks is stubbed");
  mockMermaidRender.mockResolvedValue({ svg: "<svg>diagram</svg>" });
  fileContentCache.clear();
  parseResultCache.clear();
  resolveMocks.resolve = undefined;
});

// ---------------------------------------------------------------------------
// Render path assertions — post-options-2+3
// ---------------------------------------------------------------------------

function buildBlocks(): Block[] {
  return [
    // 0: heading paragraph as html
    { id: 0, kind: "html", html: "<h1>Intro</h1>" },
    // 1: list with an internal link AND an inline code-wrap (top-level
    //    nested code fence — no longer an island, just inert DOM + a
    //    sibling copy-btn the container's onClick delegates to)
    {
      id: 1,
      kind: "html",
      html:
        "<ul>" +
        "<li>first</li>" +
        '<li>with <a href="./other.md" data-internal-href="/repo/docs/other.md">link</a></li>' +
        "<li>nested" +
        '<div class="code-wrap">' +
        '<pre><code class="language-ts hljs"><span class="hljs-keyword">const</span> x = 1;</code></pre>' +
        '<button type="button" class="copy-btn" aria-label="Copy code"></button>' +
        "</div>" +
        "</li>" +
        "</ul>",
    },
    // 2: nested mermaid placeholder inside an html block (blockquote)
    {
      id: 2,
      kind: "html",
      html: '<blockquote><span data-island-id="3"></span></blockquote>',
    },
    // 3: mermaid block whose placeholder lives in block id=2
    { id: 3, kind: "mermaid", code: "flowchart LR\n  A --> B" },
    // 4: paragraph with inline code — exercises triple-click-on-<code>
    { id: 4, kind: "html", html: "<p>text before <code>inline</code> text after</p>" },
  ];
}

const STUB_PERF = { perf: { parseMs: 1, runMs: 1, splitMs: 0, postAt: 0 }, receiveAt: 0 };

function resolveBlocks(blocks: Block[]) {
  resolveMocks.resolve!({ blocks, ...STUB_PERF });
}

async function resolveBlocksAnd(container: HTMLElement, blocks: Block[]) {
  await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
  await act(async () => resolveBlocks(blocks));
  await waitFor(() =>
    expect(container.querySelectorAll(".md-viewer-prose > section").length).toBe(4),
  );
}

/** Trigger a live-update cycle: clear resolve mock, set new file content, rerender with bumped refreshKey. */
async function triggerLiveUpdate(
  rerender: (ui: React.ReactElement) => void,
  filePath: string,
  content: string,
  refreshKey: number,
) {
  resolveMocks.resolve = undefined;
  mockReadFile.mockResolvedValueOnce(content);
  await act(async () => {
    rerender(<MarkdownViewer filePath={filePath} refreshKey={refreshKey} teamContentPath="" />);
  });
  await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
}

describe("MarkdownViewer — blocks render mode (options 2 + 3)", () => {
  it.each(["# Loaded page", ""])("keeps a pending read visible until %j resolves", async (text) => {
    let finish!: (content: string) => void;
    mockReadFile.mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    const { container } = render(<MarkdownViewer filePath="/repo/pending.md" teamContentPath="" />);
    expect(container.textContent).toContain("Loading...");
    await act(async () => finish(text));
    if (text) {
      expect(container.textContent).toContain("Loading...");
      await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
      await act(async () => resolveBlocks([{ id: 0, kind: "html", html: "<h1>Loaded page</h1>" }]));
      expect(container.textContent).toContain("Loaded page");
    }
    expect(container.textContent).not.toContain("Loading...");
    expect(container.textContent).not.toContain("Failed to load file");
  });

  it("shows loading when a zero-byte page switches to another pending file", async () => {
    mockReadFile.mockResolvedValueOnce("");
    const { container, rerender } = render(
      <MarkdownViewer filePath="/repo/empty.md" teamContentPath="" />,
    );
    await act(async () => {});
    expect(container.textContent).not.toContain("Loading...");
    let finish!: (content: string) => void;
    mockReadFile.mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    rerender(<MarkdownViewer filePath="/repo/another.md" teamContentPath="" />);
    expect(container.textContent).toContain("Loading...");
    await act(async () => finish(""));
    expect(container.textContent).not.toContain("Loading...");
  });

  it("shows loading while retrying a failed read of the same file", async () => {
    mockReadFile.mockRejectedValueOnce(new Error("Permission denied"));
    const { container, rerender } = render(
      <MarkdownViewer filePath="/repo/retry.md" refreshKey={0} teamContentPath="" />,
    );
    await waitFor(() => expect(container.textContent).toContain("Failed to load file"));
    let finish!: (content: string) => void;
    mockReadFile.mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    rerender(<MarkdownViewer filePath="/repo/retry.md" refreshKey={1} teamContentPath="" />);
    expect(container.textContent).toContain("Loading...");
    await act(async () => finish(""));
    expect(container.textContent).not.toContain("Loading...");
    expect(container.textContent).not.toContain("Failed to load file");
  });

  it("renders every html / table block as a <section> in .md-viewer-prose", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const prose = container.querySelector(".md-viewer-prose")!;
    const sections = prose.querySelectorAll(":scope > section");
    // 4 html blocks; mermaid block renders null in the list (mounted via
    // its placeholder in block #2).
    expect(sections).toHaveLength(4);
    expect(sections[0].innerHTML).toBe("<h1>Intro</h1>");
    expect(sections[1].innerHTML).toContain("<ul>");
    expect(sections[2].innerHTML).toContain("<blockquote>");
    expect(sections[3].innerHTML).toContain("<code>inline</code>");
  });

  it("SR-8 regression: block content survives React.StrictMode's mount→unmount→remount (self-clearing ref must not strand blank sections)", async () => {
    // Repro of the 'content flashes and disappears' bug: the app renders under
    // <StrictMode>, which double-invokes effects (setup → cleanup → setup) on
    // every mount in dev. `useSelfClearingRef`'s cleanup blanks the <section>'s
    // innerHTML on the simulated unmount, but `prevHtmlRef` (a ref) survives the
    // remount — so BlockSection's `prevHtmlRef.current === html` early-return
    // then skips the repaint and the section stays permanently empty. The fix
    // treats an emptied container as a fresh paint, so content must be present.
    const { container } = await act(async () => {
      return render(
        <StrictMode>
          <MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />
        </StrictMode>,
      );
    });
    await resolveBlocksAnd(container, buildBlocks());

    const sections = container.querySelectorAll(".md-viewer-prose > section");
    expect(sections).toHaveLength(4);
    // Without the fix these are all "" after the StrictMode remount clear.
    expect(sections[0].innerHTML).toBe("<h1>Intro</h1>");
    expect(sections[1].innerHTML).toContain("<ul>");
    expect(sections[3].innerHTML).toContain("<code>inline</code>");
  });

  it("inline code fence lands in the DOM with a .code-wrap + .copy-btn; no React island mount needed", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const wrap = container.querySelector(".code-wrap")!;
    expect(wrap).not.toBeNull();
    const codeEl = wrap.querySelector("pre code");
    expect(codeEl?.textContent).toContain("const x = 1;");
    expect(wrap.querySelector("button.copy-btn")).not.toBeNull();
  });

  it("delegated click on button.copy-btn writes the adjacent <pre><code> text to the clipboard", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });

    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const copyBtn = container.querySelector<HTMLButtonElement>("button.copy-btn")!;
    await act(async () => {
      fireEvent.click(copyBtn);
    });
    expect(writeText).toHaveBeenCalledWith("const x = 1;");
    // Wait for the class flip to land after clipboard resolves.
    await waitFor(() => expect(copyBtn.classList.contains("copied")).toBe(true));
  });

  it("delegated click on a[data-internal-href] fires onNavigate with the resolved path", async () => {
    const onNavigate = vi.fn();
    const { container } = await act(async () => {
      return render(
        <MarkdownViewer
          filePath="/repo/docs/intro.md"
          onNavigate={onNavigate}
          teamContentPath=""
        />,
      );
    });
    await resolveBlocksAnd(container, buildBlocks());

    const link = container.querySelector<HTMLAnchorElement>("a[data-internal-href]")!;
    await act(async () => {
      fireEvent.click(link);
    });
    expect(onNavigate).toHaveBeenCalledWith("/repo/docs/other.md");
  });

  // Code-review regression guards below:

  it("empty markdown content clears Loading... — does not hang on zero-byte files", async () => {
    // Regression: effect 2's early-return branch for `!content` used to
    // skip `setLoading(false)`, stranding the viewer on the Loading…
    // screen whenever a zero-byte (or transient-empty) file was opened.
    mockReadFile.mockResolvedValueOnce("");
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/empty.md" teamContentPath="" />);
    });
    await waitFor(() => {
      expect(container.textContent).not.toContain("Loading");
    });
  });

  it("click on an internal .md anchor always preventDefaults, even when onNavigate is absent", async () => {
    // Regression: absent-handler path used to early-return without
    // preventDefault, letting the browser follow the original href and
    // navigate the entire Tauri webview off the viewer.
    const { container } = await act(async () => {
      // Intentionally NO onNavigate prop.
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const link = container.querySelector<HTMLAnchorElement>("a[data-internal-href]")!;
    const clickEvent = new MouseEvent("click", { bubbles: true, cancelable: true });
    let defaulted = true;
    // Simulate dispatch; React adapts native events into synthetic ones
    // via the onClick delegation, which calls preventDefault on the
    // synthetic layer. Use fireEvent to route through React's handlers.
    await act(async () => {
      defaulted = fireEvent(link, clickEvent);
    });
    // fireEvent returns false when preventDefault was called.
    expect(defaulted).toBe(false);
  });

  it("copy click survives absent navigator.clipboard without crashing", async () => {
    // Regression: before the `?.` + `.catch()` patch, a missing
    // clipboard API (insecure context, denied permission) would throw
    // synchronously from the delegated handler.
    vi.stubGlobal("navigator", {});
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const copyBtn = container.querySelector<HTMLButtonElement>("button.copy-btn")!;
    // No throw when clipboard is unavailable.
    expect(() => {
      act(() => {
        fireEvent.click(copyBtn);
      });
    }).not.toThrow();
    // Class should NOT be added — feedback only happens on success.
    expect(copyBtn.classList.contains("copied")).toBe(false);
  });

  it("triple-click selects the nearest block element, not the whole prose container", async () => {
    // Regression: `.md-viewer-prose > section { display: contents }` was
    // causing the browser's paragraph-boundary walk to land on
    // .md-viewer-prose, selecting the entire document. The mousedown
    // handler now intercepts detail === 3 and selects the nearest real
    // block (intercepted on mousedown so there's no flash of the native
    // wrong selection before our override commits).
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const firstListItem = container.querySelector<HTMLElement>("ul > li")!;
    expect(firstListItem.textContent).toBe("first");

    await act(async () => {
      fireEvent.mouseDown(firstListItem, { detail: 3 });
    });

    const selection = window.getSelection()!;
    expect(selection.rangeCount).toBe(1);
    const range = selection.getRangeAt(0);
    // The range should span exactly the <li>'s contents, nothing else.
    expect(range.startContainer).toBe(firstListItem);
    expect(range.endContainer).toBe(firstListItem);
    expect(range.toString()).toBe("first");
  });

  it("triple-click on a code fence selects the <code> contents", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const codeEl = container.querySelector<HTMLElement>(".code-wrap pre > code")!;
    // Click on a syntax-highlight span inside the code — target walks up
    // through <span> → <code>. <code> is first in the selector list so
    // that's what gets selected (range contents match either way).
    const innerSpan = codeEl.querySelector<HTMLElement>("span.hljs-keyword")!;
    await act(async () => {
      fireEvent.mouseDown(innerSpan, { detail: 3 });
    });

    const selection = window.getSelection()!;
    const range = selection.getRangeAt(0);
    expect(range.startContainer).toBe(codeEl);
    expect(range.endContainer).toBe(codeEl);
    expect(range.toString()).toContain("const x = 1;");
  });

  it("triple-click on an inline <code> selects just the code, not the enclosing <p>", async () => {
    // Inline `code` (single-backtick) should select its own content on
    // triple-click — not the whole paragraph that wraps it.
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const paragraphs = Array.from(container.querySelectorAll<HTMLElement>("p"));
    const codeEl = paragraphs
      .map((p) => p.querySelector<HTMLElement>("code"))
      .find((c): c is HTMLElement => c !== null)!;
    expect(codeEl.textContent).toBe("inline");

    await act(async () => {
      fireEvent.mouseDown(codeEl, { detail: 3 });
    });

    const selection = window.getSelection()!;
    const range = selection.getRangeAt(0);
    expect(range.startContainer).toBe(codeEl);
    expect(range.endContainer).toBe(codeEl);
    expect(range.toString()).toBe("inline");
  });

  it("Cmd+ArrowDown scrolls the viewer to the bottom; Cmd+ArrowUp scrolls to the top (smooth)", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const scrollEl = container.querySelector<HTMLDivElement>(".h-full.overflow-y-auto")!;
    Object.defineProperty(scrollEl, "scrollHeight", { value: 5000, configurable: true });
    Object.defineProperty(scrollEl, "offsetParent", { value: document.body, configurable: true });
    const scrollToSpy = vi.fn();
    scrollEl.scrollTo = scrollToSpy as unknown as typeof scrollEl.scrollTo;

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", metaKey: true }));
    });
    expect(scrollToSpy).toHaveBeenLastCalledWith({ top: 5000, behavior: "smooth" });

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp", metaKey: true }));
    });
    expect(scrollToSpy).toHaveBeenLastCalledWith({ top: 0, behavior: "smooth" });
  });

  it("plain ArrowDown scrolls by one line; ArrowUp scrolls up by one line", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const scrollEl = container.querySelector<HTMLDivElement>(".h-full.overflow-y-auto")!;
    Object.defineProperty(scrollEl, "scrollHeight", { value: 5000, configurable: true });
    Object.defineProperty(scrollEl, "clientHeight", { value: 600, configurable: true });
    Object.defineProperty(scrollEl, "offsetParent", { value: document.body, configurable: true });
    const scrollToSpy = vi.fn();
    scrollEl.scrollTo = scrollToSpy as unknown as typeof scrollEl.scrollTo;
    scrollEl.scrollTop = 200;

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown" }));
    });
    expect(scrollToSpy).toHaveBeenLastCalledWith({ top: 260, behavior: "smooth" });

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp" }));
    });
    expect(scrollToSpy).toHaveBeenLastCalledWith({ top: 140, behavior: "smooth" });
  });

  it("PageDown scrolls ~one viewport; Home scrolls to 0; End scrolls to bottom", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const scrollEl = container.querySelector<HTMLDivElement>(".h-full.overflow-y-auto")!;
    Object.defineProperty(scrollEl, "scrollHeight", { value: 5000, configurable: true });
    Object.defineProperty(scrollEl, "clientHeight", { value: 600, configurable: true });
    Object.defineProperty(scrollEl, "offsetParent", { value: document.body, configurable: true });
    const scrollToSpy = vi.fn();
    scrollEl.scrollTo = scrollToSpy as unknown as typeof scrollEl.scrollTo;
    scrollEl.scrollTop = 1000;

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "PageDown" }));
    });
    // pageStep = clientHeight - 80 = 520. 1000 + 520 = 1520.
    expect(scrollToSpy).toHaveBeenLastCalledWith({ top: 1520, behavior: "smooth" });

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Home" }));
    });
    expect(scrollToSpy).toHaveBeenLastCalledWith({ top: 0, behavior: "smooth" });

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "End" }));
    });
    expect(scrollToSpy).toHaveBeenLastCalledWith({ top: 5000, behavior: "smooth" });
  });

  it("Cmd+A selects only the prose contents, not the whole document", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const scrollEl = container.querySelector<HTMLDivElement>(".h-full.overflow-y-auto")!;
    Object.defineProperty(scrollEl, "offsetParent", { value: document.body, configurable: true });
    const prose = container.querySelector<HTMLElement>(".md-viewer-prose")!;

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "a", metaKey: true }));
    });

    const selection = window.getSelection()!;
    expect(selection.rangeCount).toBe(1);
    const range = selection.getRangeAt(0);
    expect(range.startContainer).toBe(prose);
    expect(range.endContainer).toBe(prose);
  });

  it("triple-click on a paragraph containing a link does NOT navigate after selecting", async () => {
    // Regression: mousedown selected the <li>, but the synthetic click
    // that followed still matched the a[data-internal-href] branch of
    // the click handler and fired onNavigate — selection + navigation
    // in one gesture. Click handler now bails on `e.detail >= 3`.
    const onNavigate = vi.fn();
    const { container } = await act(async () => {
      return render(
        <MarkdownViewer
          filePath="/repo/docs/intro.md"
          onNavigate={onNavigate}
          teamContentPath=""
        />,
      );
    });
    await resolveBlocksAnd(container, buildBlocks());

    const li = container.querySelectorAll<HTMLElement>("ul > li")[1]!;
    expect(li.querySelector("a[data-internal-href]")).not.toBeNull();

    await act(async () => {
      fireEvent.mouseDown(li, { detail: 3 });
      fireEvent.click(li, { detail: 3 });
    });

    expect(onNavigate).not.toHaveBeenCalled();
    const selection = window.getSelection()!;
    expect(selection.getRangeAt(0).toString()).toContain("with");
  });

  it("Cmd+Shift+ArrowDown does NOT scroll — passes through so native select-to-end works", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const scrollEl = container.querySelector<HTMLDivElement>(".h-full.overflow-y-auto")!;
    Object.defineProperty(scrollEl, "scrollHeight", { value: 5000, configurable: true });
    Object.defineProperty(scrollEl, "offsetParent", { value: document.body, configurable: true });
    const scrollToSpy = vi.fn();
    scrollEl.scrollTo = scrollToSpy as unknown as typeof scrollEl.scrollTo;

    await act(async () => {
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowDown", metaKey: true, shiftKey: true }),
      );
    });
    expect(scrollToSpy).not.toHaveBeenCalled();
  });

  it("Shift+ArrowDown does NOT scroll — lets native selection-extend work", async () => {
    // Regression: round-1 CR patch only added !e.shiftKey to the cmd+arrow
    // branches; the plain-arrow branches still intercepted Shift+Arrow
    // and broke keyboard text-selection-extend.
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const scrollEl = container.querySelector<HTMLDivElement>(".h-full.overflow-y-auto")!;
    Object.defineProperty(scrollEl, "scrollHeight", { value: 5000, configurable: true });
    Object.defineProperty(scrollEl, "clientHeight", { value: 600, configurable: true });
    Object.defineProperty(scrollEl, "offsetParent", { value: document.body, configurable: true });
    const scrollToSpy = vi.fn();
    scrollEl.scrollTo = scrollToSpy as unknown as typeof scrollEl.scrollTo;

    for (const key of ["ArrowDown", "ArrowUp", "PageDown", "PageUp", "Home", "End"]) {
      await act(async () => {
        document.dispatchEvent(new KeyboardEvent("keydown", { key, shiftKey: true }));
      });
    }
    expect(scrollToSpy).not.toHaveBeenCalled();
  });

  it("Cmd+A preventDefaults in the error render branch (no prose mounted)", async () => {
    // Regression: the round-1 and round-2 patches both left a gate
    // (`!proseRef.current`) that short-circuited before preventDefault
    // during render paths that don't mount the prose container —
    // Failed-to-load, the deleted-file view, the initial empty
    // placeholder. The webview's native selectAll would then grab the
    // whole document (sidebar + terminal + scratch note). preventDefault
    // must run unconditionally when this handler owns the keystroke
    // (pane === file, no input focus), whether or not prose has mounted.
    mockReadFile.mockRejectedValueOnce(new Error("boom"));
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/error.md" teamContentPath="" />);
    });
    await waitFor(() => {
      expect(container.textContent).toContain("Failed to load");
      expect(container.querySelector(".md-viewer-prose")).toBeNull();
    });

    const keyEvent = new KeyboardEvent("keydown", {
      key: "a",
      metaKey: true,
      cancelable: true,
    });
    await act(async () => {
      document.dispatchEvent(keyEvent);
    });
    expect(keyEvent.defaultPrevented).toBe(true);
  });

  it("Cmd+A on a loaded viewer selects the prose contents only", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const prose = container.querySelector<HTMLElement>(".md-viewer-prose")!;
    Object.defineProperty(prose, "offsetParent", { value: document.body, configurable: true });

    const keyEvent = new KeyboardEvent("keydown", {
      key: "a",
      metaKey: true,
      cancelable: true,
    });
    await act(async () => {
      document.dispatchEvent(keyEvent);
    });
    expect(keyEvent.defaultPrevented).toBe(true);
    const selection = window.getSelection()!;
    expect(selection.rangeCount).toBe(1);
    expect(selection.getRangeAt(0).startContainer).toBe(prose);
  });

  it("Cmd+ArrowUp/Down is a no-op when focus is in a text input", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const scrollEl = container.querySelector<HTMLDivElement>(".h-full.overflow-y-auto")!;
    Object.defineProperty(scrollEl, "offsetParent", { value: document.body, configurable: true });
    const scrollToSpy = vi.fn();
    scrollEl.scrollTo = scrollToSpy as unknown as typeof scrollEl.scrollTo;

    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", metaKey: true }));
    });
    expect(scrollToSpy).not.toHaveBeenCalled();

    input.remove();
  });

  it("caches the Block[] and skips the worker on re-mount of the same content", async () => {
    const { unmount } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    const blocks = buildBlocks();
    await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
    await act(async () => {
      resolveBlocks(blocks);
    });

    expect(
      parseResultCache.get("/repo/docs/intro.md", "irrelevant — parseToBlocks is stubbed"),
    ).toBeDefined();

    unmount();
    resolveMocks.resolve = undefined;
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await waitFor(() =>
      expect(container.querySelectorAll(".md-viewer-prose > section").length).toBe(4),
    );
    // Cache hit: parseToBlocks was not called again, so `resolve` stays undefined.
    expect(resolveMocks.resolve).toBeUndefined();
  });

  // -------------------------------------------------------------------------
  // Story 32.7: anchor scroll + modifier-click + middle-click + file+hash
  // -------------------------------------------------------------------------

  it("delegated click on a hash anchor calls scrollIntoView on matching id and preventDefaults", async () => {
    const blocks: Block[] = [
      {
        id: 0,
        kind: "html",
        html: '<h2 id="what-makes-this-special">What</h2><p><a href="#what-makes-this-special">ToC</a></p>',
      },
    ];
    const MarkdownViewer = (await import("./markdown-viewer")).MarkdownViewer;
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
    await act(async () => {
      resolveBlocks(blocks);
    });
    await waitFor(() =>
      expect(container.querySelectorAll(".md-viewer-prose > section").length).toBe(1),
    );

    const target = container.querySelector<HTMLHeadingElement>("#what-makes-this-special")!;
    const scrollSpy = vi.spyOn(target, "scrollIntoView").mockImplementation(() => {});

    const link = container.querySelector<HTMLAnchorElement>('a[href="#what-makes-this-special"]')!;
    let defaultPrevented = false;
    const origPreventDefault = Event.prototype.preventDefault;
    Event.prototype.preventDefault = function () {
      defaultPrevented = true;
      return origPreventDefault.call(this);
    };
    try {
      await act(async () => {
        fireEvent.click(link);
      });
    } finally {
      Event.prototype.preventDefault = origPreventDefault;
    }
    expect(scrollSpy).toHaveBeenCalledWith({ behavior: "instant", block: "start" });
    expect(defaultPrevented).toBe(true);
  });

  it("Cmd+click on an internal link fires onNavigate with openInNewTab: true", async () => {
    const onNavigate = vi.fn();
    const { container } = await act(async () => {
      return render(
        <MarkdownViewer
          filePath="/repo/docs/intro.md"
          onNavigate={onNavigate}
          teamContentPath=""
        />,
      );
    });
    await resolveBlocksAnd(container, buildBlocks());

    const link = container.querySelector<HTMLAnchorElement>("a[data-internal-href]")!;
    await act(async () => {
      fireEvent.click(link, { metaKey: true });
    });
    expect(onNavigate).toHaveBeenCalledWith("/repo/docs/other.md", { openInNewTab: true });
  });

  it("Ctrl+click on an internal link fires onNavigate with openInNewTab: true", async () => {
    const onNavigate = vi.fn();
    const { container } = await act(async () => {
      return render(
        <MarkdownViewer
          filePath="/repo/docs/intro.md"
          onNavigate={onNavigate}
          teamContentPath=""
        />,
      );
    });
    await resolveBlocksAnd(container, buildBlocks());

    const link = container.querySelector<HTMLAnchorElement>("a[data-internal-href]")!;
    await act(async () => {
      fireEvent.click(link, { ctrlKey: true });
    });
    expect(onNavigate).toHaveBeenCalledWith("/repo/docs/other.md", { openInNewTab: true });
  });

  it("middle-click (auxclick button 1) on an internal link fires onNavigate with openInNewTab: true", async () => {
    const onNavigate = vi.fn();
    const { container } = await act(async () => {
      return render(
        <MarkdownViewer
          filePath="/repo/docs/intro.md"
          onNavigate={onNavigate}
          teamContentPath=""
        />,
      );
    });
    await resolveBlocksAnd(container, buildBlocks());

    const link = container.querySelector<HTMLAnchorElement>("a[data-internal-href]")!;
    await act(async () => {
      fireEvent(link, new MouseEvent("auxclick", { button: 1, bubbles: true, cancelable: true }));
    });
    expect(onNavigate).toHaveBeenCalledWith("/repo/docs/other.md", { openInNewTab: true });
  });

  it("auxclick with right-button (button 2) does NOT fire onNavigate", async () => {
    const onNavigate = vi.fn();
    const { container } = await act(async () => {
      return render(
        <MarkdownViewer
          filePath="/repo/docs/intro.md"
          onNavigate={onNavigate}
          teamContentPath=""
        />,
      );
    });
    await resolveBlocksAnd(container, buildBlocks());

    const link = container.querySelector<HTMLAnchorElement>("a[data-internal-href]")!;
    await act(async () => {
      fireEvent(link, new MouseEvent("auxclick", { button: 2, bubbles: true, cancelable: true }));
    });
    expect(onNavigate).not.toHaveBeenCalled();
  });

  it("click on an internal link with a fragment forwards the hash through options", async () => {
    const onNavigate = vi.fn();
    const blocks: Block[] = [
      {
        id: 0,
        kind: "html",
        html: '<p><a href="./other.md#section" data-internal-href="/repo/docs/other.md">link</a></p>',
      },
    ];
    const { container } = await act(async () => {
      return render(
        <MarkdownViewer
          filePath="/repo/docs/intro.md"
          onNavigate={onNavigate}
          teamContentPath=""
        />,
      );
    });
    await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
    await act(async () => {
      resolveBlocks(blocks);
    });
    await waitFor(() =>
      expect(container.querySelectorAll(".md-viewer-prose > section").length).toBe(1),
    );

    const link = container.querySelector<HTMLAnchorElement>("a[data-internal-href]")!;
    await act(async () => {
      fireEvent.click(link);
    });
    expect(onNavigate).toHaveBeenCalledWith("/repo/docs/other.md", { hash: "#section" });
  });

  it("Cmd+click on internal link with fragment forwards BOTH openInNewTab and hash", async () => {
    const onNavigate = vi.fn();
    const blocks: Block[] = [
      {
        id: 0,
        kind: "html",
        html: '<p><a href="./other.md#section" data-internal-href="/repo/docs/other.md">link</a></p>',
      },
    ];
    const { container } = await act(async () => {
      return render(
        <MarkdownViewer
          filePath="/repo/docs/intro.md"
          onNavigate={onNavigate}
          teamContentPath=""
        />,
      );
    });
    await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
    await act(async () => {
      resolveBlocks(blocks);
    });
    await waitFor(() =>
      expect(container.querySelectorAll(".md-viewer-prose > section").length).toBe(1),
    );

    const link = container.querySelector<HTMLAnchorElement>("a[data-internal-href]")!;
    await act(async () => {
      fireEvent.click(link, { metaKey: true });
    });
    expect(onNavigate).toHaveBeenCalledWith("/repo/docs/other.md", {
      openInNewTab: true,
      hash: "#section",
    });
  });

  it("Alt+ArrowDown scrolls by LINE_STEP * 10 (fast scroll)", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const scrollEl = container.querySelector<HTMLDivElement>(".h-full.overflow-y-auto")!;
    Object.defineProperty(scrollEl, "scrollHeight", { value: 5000, configurable: true });
    Object.defineProperty(scrollEl, "clientHeight", { value: 600, configurable: true });
    Object.defineProperty(scrollEl, "offsetParent", { value: document.body, configurable: true });
    Object.defineProperty(scrollEl, "scrollTop", {
      value: 1000,
      configurable: true,
      writable: true,
    });
    const scrollToSpy = vi.fn();
    scrollEl.scrollTo = scrollToSpy as unknown as typeof scrollEl.scrollTo;

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", altKey: true }));
    });
    // LINE_STEP = 60, 10x = 600. scrollTop (1000) + 600 = 1600.
    expect(scrollToSpy).toHaveBeenCalledWith({ top: 1600, behavior: "smooth" });
  });

  it("Alt+ArrowUp scrolls UP by LINE_STEP * 10", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const scrollEl = container.querySelector<HTMLDivElement>(".h-full.overflow-y-auto")!;
    Object.defineProperty(scrollEl, "scrollHeight", { value: 5000, configurable: true });
    Object.defineProperty(scrollEl, "clientHeight", { value: 600, configurable: true });
    Object.defineProperty(scrollEl, "offsetParent", { value: document.body, configurable: true });
    Object.defineProperty(scrollEl, "scrollTop", {
      value: 1000,
      configurable: true,
      writable: true,
    });
    const scrollToSpy = vi.fn();
    scrollEl.scrollTo = scrollToSpy as unknown as typeof scrollEl.scrollTo;

    await act(async () => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp", altKey: true }));
    });
    expect(scrollToSpy).toHaveBeenCalledWith({ top: 400, behavior: "smooth" });
  });

  it("onContentCommitted fires after blocks commit — not on refreshKey bump alone", async () => {
    // Regression guard: refreshKey bumps synchronously on file-change
    // events, but the actual DOM swap requires an async worker round-
    // trip. Consumers (e.g. FindBar) need the post-commit signal.
    const onContentCommitted = vi.fn();
    const { container } = await act(async () => {
      return render(
        <MarkdownViewer
          filePath="/repo/docs/intro.md"
          onContentCommitted={onContentCommitted}
          teamContentPath=""
        />,
      );
    });
    // Before the worker resolves, onContentCommitted must NOT fire.
    expect(onContentCommitted).not.toHaveBeenCalled();

    await resolveBlocksAnd(container, buildBlocks());

    // After blocks commit: onContentCommitted fires at least once.
    expect(onContentCommitted).toHaveBeenCalled();
  });

  it("Shift+Alt+ArrowDown does NOT scroll — existing shiftKey guard wins", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const scrollEl = container.querySelector<HTMLDivElement>(".h-full.overflow-y-auto")!;
    Object.defineProperty(scrollEl, "scrollHeight", { value: 5000, configurable: true });
    Object.defineProperty(scrollEl, "clientHeight", { value: 600, configurable: true });
    Object.defineProperty(scrollEl, "offsetParent", { value: document.body, configurable: true });
    const scrollToSpy = vi.fn();
    scrollEl.scrollTo = scrollToSpy as unknown as typeof scrollEl.scrollTo;

    await act(async () => {
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowDown", altKey: true, shiftKey: true }),
      );
    });
    expect(scrollToSpy).not.toHaveBeenCalled();
  });

  // -------------------------------------------------------------------------
  // F-34: DOM morphing — live update preserves unchanged DOM nodes
  // -------------------------------------------------------------------------

  it("unchanged child element within a changed block is preserved via morphdom (AC-1)", async () => {
    const initialBlocks: Block[] = [
      { id: 0, kind: "html", html: '<p id="stable">unchanged</p><p id="dynamic">original</p>' },
      { id: 1, kind: "html", html: "<h2>untouched</h2>" },
    ];

    const { container, rerender } = await act(async () => {
      return render(
        <MarkdownViewer filePath="/repo/docs/morph.md" refreshKey={0} teamContentPath="" />,
      );
    });
    await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
    await act(async () => resolveBlocks(initialBlocks));
    await waitFor(() =>
      expect(container.querySelectorAll(".md-viewer-prose > section").length).toBe(2),
    );

    const stablePara = container.querySelector<HTMLElement>("#stable")!;
    expect(stablePara.textContent).toBe("unchanged");
    const untouchedH2 = container
      .querySelectorAll(".md-viewer-prose > section")[1]
      .querySelector("h2")!;

    await triggerLiveUpdate(rerender, "/repo/docs/morph.md", "different content", 1);

    const updatedBlocks: Block[] = [
      { id: 0, kind: "html", html: '<p id="stable">unchanged</p><p id="dynamic">modified</p>' },
      { id: 1, kind: "html", html: "<h2>untouched</h2>" },
    ];
    await act(async () => resolveBlocks(updatedBlocks));
    await waitFor(() => expect(container.querySelector("#dynamic")?.textContent).toBe("modified"));

    // AC-1: unchanged child within a CHANGED block → same DOM instance
    expect(container.querySelector<HTMLElement>("#stable")).toBe(stablePara);
    // AC-1: unchanged block's children → same DOM instance
    expect(container.querySelectorAll(".md-viewer-prose > section")[1].querySelector("h2")).toBe(
      untouchedH2,
    );
  });

  it("scrollTop restored after blocks re-render for same-file live update (AC-2)", async () => {
    const { container, rerender } = await act(async () => {
      return render(
        <MarkdownViewer filePath="/repo/docs/scroll.md" refreshKey={0} teamContentPath="" />,
      );
    });
    await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
    await act(async () => resolveBlocks([{ id: 0, kind: "html", html: "<p>original</p>" }]));
    await waitFor(() =>
      expect(container.querySelectorAll(".md-viewer-prose > section").length).toBe(1),
    );

    const scrollEl = container.querySelector<HTMLDivElement>(".h-full.overflow-y-auto")!;
    let currentScrollTop = 0;
    Object.defineProperty(scrollEl, "scrollTop", {
      get: () => currentScrollTop,
      set: (v: number) => {
        currentScrollTop = v;
      },
      configurable: true,
    });
    currentScrollTop = 500;

    await triggerLiveUpdate(rerender, "/repo/docs/scroll.md", "changed content", 1);
    currentScrollTop = 0; // simulate layout shift

    await act(async () => resolveBlocks([{ id: 0, kind: "html", html: "<p>updated</p>" }]));

    // AC-2: scrollTop should be restored to pre-update value
    expect(currentScrollTop).toBe(500);
  });

  it("diagram islands survive non-diagram block updates — observer stays connected (AC-3)", async () => {
    // The diagram island effect depends on `diagramFingerprint`, not the
    // `blocks` array reference. When only non-diagram blocks change, the
    // fingerprint is identical → effect does NOT re-run → existing React
    // roots and observer entries survive.
    //
    // We test this by verifying that the placeholder element (which holds
    // the diagram root) is the SAME DOM node before and after the update.
    // If the effect tore down and remounted, the placeholder would be
    // destroyed by morphdom (since it's inside an html block whose HTML
    // string is identical — morphdom preserves it) but the root would be
    // a new one. We also verify mermaid.render isn't called again.
    const initialBlocks: Block[] = [
      { id: 0, kind: "html", html: "<p>text</p>" },
      {
        id: 1,
        kind: "html",
        html: '<blockquote><span data-island-id="2"></span></blockquote>',
      },
      { id: 2, kind: "mermaid", code: "flowchart LR\n  A --> B" },
    ];

    const { container, rerender } = await act(async () => {
      return render(
        <MarkdownViewer filePath="/repo/docs/diagram.md" refreshKey={0} teamContentPath="" />,
      );
    });
    await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
    await act(async () => resolveBlocks(initialBlocks));
    await waitFor(() =>
      expect(container.querySelectorAll(".md-viewer-prose > section").length).toBe(2),
    );

    // Capture the placeholder element — it hosts the diagram createRoot
    const placeholder = container.querySelector<HTMLElement>("[data-island-id='2']")!;
    expect(placeholder).not.toBeNull();

    // Record mermaid.render calls so far (may be 0 or 1 depending on
    // React scheduling of the separate island root — that's OK, we only
    // care that it doesn't get called AGAIN after the update).
    const renderCountBefore = mockMermaidRender.mock.calls.length;

    // Trigger update — only block 0 (text) changes, diagram is unchanged
    await triggerLiveUpdate(rerender, "/repo/docs/diagram.md", "new text", 1);

    const updatedBlocks: Block[] = [
      { id: 0, kind: "html", html: "<p>updated text</p>" },
      {
        id: 1,
        kind: "html",
        html: '<blockquote><span data-island-id="2"></span></blockquote>',
      },
      { id: 2, kind: "mermaid", code: "flowchart LR\n  A --> B" },
    ];
    await act(async () => resolveBlocks(updatedBlocks));
    await waitFor(() => expect(container.querySelector("p")?.textContent).toBe("updated text"));

    // AC-3: placeholder is the same DOM instance — morphdom preserved it
    // and the diagram effect did NOT teardown/recreate the root.
    const placeholderAfter = container.querySelector<HTMLElement>("[data-island-id='2']")!;
    expect(placeholderAfter).toBe(placeholder);

    // AC-3: mermaid.render was NOT called again after the non-diagram update
    expect(mockMermaidRender.mock.calls.length).toBe(renderCountBefore);
  });

  it("overflow-anchor: auto is set on the scroll container (AC-4)", async () => {
    const { container } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/anchor.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const scrollEl = container.querySelector<HTMLDivElement>(".h-full.overflow-y-auto")!;
    expect(scrollEl.style.overflowAnchor).toBe("auto");
  });

  // -------------------------------------------------------------------------
  // SR-8: content-viewer re-render DOM/native-backing reclamation
  // -------------------------------------------------------------------------

  it("SR-8: blanks every <section>'s imperatively-built innerHTML on viewer unmount", async () => {
    // (U) file-switch/unmount reclamation. Each BlockSection <section> is
    // filled via innerHTML/morphdom, outside React's vdom — React removes the
    // node on unmount but does not blank the subtree, so in WKWebView its
    // native backing lingers. The shared self-clearing ref must release it.
    const { container, unmount } = await act(async () => {
      return render(<MarkdownViewer filePath="/repo/docs/intro.md" teamContentPath="" />);
    });
    await resolveBlocksAnd(container, buildBlocks());

    const sections = Array.from(
      container.querySelectorAll<HTMLElement>(".md-viewer-prose > section"),
    );
    expect(sections).toHaveLength(4);
    expect(sections[0].innerHTML).not.toBe("");

    await act(async () => unmount());

    // The `sections` references survive React's detachment, so we can inspect
    // them post-unmount: `act` flushes the hook's passive cleanup, which blanks
    // each captured (now-detached) node's innerHTML — releasing the backing.
    for (const section of sections) {
      expect(section.innerHTML).toBe("");
    }
  });

  it("SR-8: in-place block change patches via morphdom and does NOT blank the <section> (F.34 preserved)", async () => {
    // Guard against clear-on-replace: the unmount teardown must live in an
    // empty-dep effect, so a content-changing re-render still patches in place
    // (same <section> instance, same preserved child node) rather than wiping
    // and rebuilding — which would reintroduce F.34's flash and add jank.
    const { container, rerender } = await act(async () => {
      return render(
        <MarkdownViewer filePath="/repo/docs/replace.md" refreshKey={0} teamContentPath="" />,
      );
    });
    await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
    await act(async () => resolveBlocks([{ id: 0, kind: "html", html: '<p id="keep">a</p>' }]));
    await waitFor(() =>
      expect(container.querySelectorAll(".md-viewer-prose > section").length).toBe(1),
    );

    const sectionBefore = container.querySelector<HTMLElement>(".md-viewer-prose > section")!;
    const keepBefore = container.querySelector<HTMLElement>("#keep")!;

    await triggerLiveUpdate(rerender, "/repo/docs/replace.md", "changed", 1);
    await act(async () =>
      resolveBlocks([{ id: 0, kind: "html", html: '<p id="keep">a</p><p id="added">b</p>' }]),
    );
    await waitFor(() => expect(container.querySelector("#added")?.textContent).toBe("b"));

    // Same <section> instance — not blanked+replaced.
    expect(container.querySelector<HTMLElement>(".md-viewer-prose > section")).toBe(sectionBefore);
    // Unchanged child preserved by morphdom — proves no clear-on-replace wipe.
    expect(container.querySelector<HTMLElement>("#keep")).toBe(keepBefore);
  });
});

describe("caller scope through query islands", () => {
  beforeEach(() => {
    vi.stubGlobal(
      "IntersectionObserver",
      class {
        constructor(private callback: (entries: IntersectionObserverEntry[]) => void) {}
        observe(target: Element) {
          this.callback([{ target, isIntersecting: true } as IntersectionObserverEntry]);
        }
        unobserve() {}
        disconnect() {}
      },
    );
  });
  afterEach(() => vi.unstubAllGlobals());
  it("an inherited Markdown query retains its source path and uses the trusted host root", async () => {
    const source = "/org/parent/content/shared.md";
    const host = "/org/child/content";
    mockQueryContext.mockResolvedValue({
      schemaVersion: 1,
      rows: [],
      total: 0,
      indexing: false,
      generation: 1,
      textScanTruncated: false,
      totalApproximate: false,
    });
    const { unmount } = render(<MarkdownViewer filePath={source} teamContentPath={host} />);
    await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
    await act(async () =>
      resolveBlocks([
        { id: 0, kind: "html", html: '<span data-island-id="1"></span>' },
        { id: 1, kind: "csquery", code: "view: list" },
      ]),
    );
    await waitFor(() => expect(mockQueryContext).toHaveBeenCalled());
    expect(mockQueryContext).toHaveBeenCalledWith(expect.any(Object), source, undefined, host);
    unmount();
  });

  it("production viewer dispatch retains the host team independently of source root", async () => {
    const { renderViewer } = await import("@/shared/lib/render-viewer");
    const source = "/org/parent/content/shared.md";
    mockQueryContext.mockResolvedValue({
      schemaVersion: 1,
      rows: [],
      total: 0,
      indexing: false,
      generation: 1,
      textScanTruncated: false,
      totalApproximate: false,
    });
    const { unmount } = render(
      renderViewer(
        { filePath: source, teamContentPath: "/org/parent/content", hostTeamKey: "acme/ctx:child" },
        {},
      ),
    );
    await waitFor(() => expect(resolveMocks.resolve).toBeDefined());
    await act(async () =>
      resolveBlocks([
        { id: 0, kind: "html", html: '<span data-island-id="1"></span>' },
        { id: 1, kind: "csquery", code: "view: list" },
      ]),
    );
    await waitFor(() =>
      expect(mockQueryContext).toHaveBeenCalledWith(
        expect.any(Object),
        source,
        undefined,
        undefined,
        "acme/ctx:child",
      ),
    );
    unmount();
  });
});
