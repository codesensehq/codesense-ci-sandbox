// SPDX-License-Identifier: MIT
import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, act, fireEvent } from "@/test-utils";
import { MarkdownViewer } from "./markdown-viewer";
import { fileContentCache } from "@/shared/lib/file-content-cache";

// ---------------------------------------------------------------------------
// Mocks (same pattern as markdown-viewer.test.tsx)
// ---------------------------------------------------------------------------

const mockReadFile = vi.fn<(path: string) => Promise<string>>();
vi.mock("@/tauri/commands", () => ({
  readFile: (...args: unknown[]) => mockReadFile(...(args as [string])),
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

// Capture ReactMarkdown components prop to test link/pre/code handlers
let capturedComponents: Record<string, (...args: unknown[]) => unknown> = {};
vi.mock("react-markdown", () => ({
  default: ({
    children,
    components,
  }: {
    children: string;
    components?: Record<string, (...args: unknown[]) => unknown>;
  }) => {
    capturedComponents = components ?? {};
    return <div data-testid="react-markdown">{children}</div>;
  },
}));

vi.mock("remark-gfm", () => ({ default: () => {} }));
vi.mock("remark-frontmatter", () => ({ default: () => {} }));
vi.mock("rehype-highlight", () => ({ default: () => {} }));

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

beforeEach(() => {
  vi.clearAllMocks();
  mockStoreValues.contentWidth = "normal";
  mockStoreValues.appTheme = "dracula";
  mockStoreValues.viewerFontSize = "normal";
  mockStoreValues.selectedRepoPath = "/repo";
  capturedComponents = {};
  mockReadFile.mockResolvedValue("# Hello\n\nContent");
  mockMermaidRender.mockResolvedValue({ svg: "<svg>diagram</svg>" });
  // Deterministic cache state — every test starts with an empty cache
  // so readFile counts and Loading… behavior are isolated.
  fileContentCache.clear();
});

// ---------------------------------------------------------------------------
// Copy code button (lines 321-360)
// ---------------------------------------------------------------------------

describe("Copy code button in pre component", () => {
  it("copies code text to clipboard when copy button is clicked", async () => {
    const writeTextMock = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText: writeTextMock } });

    await act(async () => {
      render(<MarkdownViewer filePath="/test.md" teamContentPath="" />);
    });
    await waitFor(() => expect(capturedComponents.pre).toBeDefined());

    const PreComponent = capturedComponents.pre as (props: {
      children: React.ReactNode;
    }) => React.ReactElement;

    const { container } = render(
      <PreComponent>
        <code>console.log("hello")</code>
      </PreComponent>,
    );

    const copyBtn = container.querySelector("button")!;
    await act(async () => {
      fireEvent.click(copyBtn);
    });

    expect(writeTextMock).toHaveBeenCalled();
  });

  it("shows check icon after successful copy", async () => {
    const writeTextMock = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText: writeTextMock } });

    await act(async () => {
      render(<MarkdownViewer filePath="/test.md" teamContentPath="" />);
    });
    await waitFor(() => expect(capturedComponents.pre).toBeDefined());

    const PreComponent = capturedComponents.pre as (props: {
      children: React.ReactNode;
    }) => React.ReactElement;

    const { container } = render(
      <PreComponent>
        <code>code here</code>
      </PreComponent>,
    );

    const copyBtn = container.querySelector("button")!;
    expect(copyBtn.getAttribute("aria-label")).toBe("Copy code");

    await act(async () => {
      fireEvent.click(copyBtn);
    });

    // After copy, the check icon should appear (class text-green-500)
    await waitFor(() => {
      expect(container.querySelector(".text-green-500")).not.toBeNull();
    });
  });
});

// ---------------------------------------------------------------------------
// Internal link resolution (lines 361-430)
// ---------------------------------------------------------------------------

describe("Internal link resolution in anchor component", () => {
  it("renders external links with target=_blank", async () => {
    await act(async () => {
      render(
        <MarkdownViewer filePath="/repo/docs/readme.md" onNavigate={vi.fn()} teamContentPath="" />,
      );
    });
    await waitFor(() => expect(capturedComponents.a).toBeDefined());

    const AComponent = capturedComponents.a as (props: {
      href?: string;
      children: React.ReactNode;
    }) => React.ReactElement;

    const { container } = render(<AComponent href="https://example.com">External</AComponent>);
    const link = container.querySelector("a")!;
    expect(link.target).toBe("_blank");
    expect(link.rel).toContain("noopener");
  });

  it("renders anchor-only links as plain anchors", async () => {
    await act(async () => {
      render(
        <MarkdownViewer filePath="/repo/docs/readme.md" onNavigate={vi.fn()} teamContentPath="" />,
      );
    });
    await waitFor(() => expect(capturedComponents.a).toBeDefined());

    const AComponent = capturedComponents.a as (props: {
      href?: string;
      children: React.ReactNode;
    }) => React.ReactElement;

    const { container } = render(<AComponent href="#section">Anchor</AComponent>);
    const link = container.querySelector("a")!;
    expect(link.getAttribute("href")).toBe("#section");
    expect(link.target).not.toBe("_blank");
  });

  it("resolves relative .md links and calls onNavigate", async () => {
    const onNavigate = vi.fn();
    await act(async () => {
      render(
        <MarkdownViewer
          filePath="/repo/docs/readme.md"
          onNavigate={onNavigate}
          teamContentPath=""
        />,
      );
    });
    await waitFor(() => expect(capturedComponents.a).toBeDefined());

    const AComponent = capturedComponents.a as (props: {
      href?: string;
      children: React.ReactNode;
    }) => React.ReactElement;

    const { container } = render(<AComponent href="../other/file.md">Link</AComponent>);
    const link = container.querySelector("a")!;

    await act(async () => {
      fireEvent.click(link);
    });

    // /repo/docs + ../other/file.md -> /repo/other/file.md
    expect(onNavigate).toHaveBeenCalledWith("/repo/other/file.md");
  });

  it("resolves absolute .md links relative to content/ root (PTS-2)", async () => {
    const onNavigate = vi.fn();
    await act(async () => {
      render(
        <MarkdownViewer
          filePath="/repo/content/planning-artifacts/arch.md"
          onNavigate={onNavigate}
          teamContentPath=""
        />,
      );
    });
    await waitFor(() => expect(capturedComponents.a).toBeDefined());

    const AComponent = capturedComponents.a as (props: {
      href?: string;
      children: React.ReactNode;
    }) => React.ReactElement;

    const { container } = render(<AComponent href="/other/spec.md">Link</AComponent>);
    const link = container.querySelector("a")!;

    await act(async () => {
      fireEvent.click(link);
    });

    // Anchor strips at `/content/`; absolute href is appended to the prefix.
    expect(onNavigate).toHaveBeenCalledWith("/repo/other/spec.md");
  });

  it("renders fallback anchor when href has no special handling", async () => {
    await act(async () => {
      render(
        <MarkdownViewer filePath="/repo/docs/readme.md" onNavigate={vi.fn()} teamContentPath="" />,
      );
    });
    await waitFor(() => expect(capturedComponents.a).toBeDefined());

    const AComponent = capturedComponents.a as (props: {
      href?: string;
      children: React.ReactNode;
    }) => React.ReactElement;

    // Non-md, non-http, non-anchor link
    const { container } = render(<AComponent href="image.png">Img</AComponent>);
    const link = container.querySelector("a")!;
    expect(link.getAttribute("href")).toBe("image.png");
  });

  it("renders children when href is undefined", async () => {
    await act(async () => {
      render(<MarkdownViewer filePath="/test.md" onNavigate={vi.fn()} teamContentPath="" />);
    });
    await waitFor(() => expect(capturedComponents.a).toBeDefined());

    const AComponent = capturedComponents.a as (props: {
      href?: string;
      children: React.ReactNode;
    }) => React.ReactElement;

    const { container } = render(<AComponent>No href</AComponent>);
    expect(container.textContent).toBe("No href");
  });
});

// ---------------------------------------------------------------------------
// Loading state (line 245-250)
// ---------------------------------------------------------------------------

describe("Loading state", () => {
  it("shows loading indicator while file is being read", async () => {
    // Make readFile hang so loading state is visible
    mockReadFile.mockReturnValue(new Promise(() => {}));

    await act(async () => {
      render(<MarkdownViewer filePath="/test.md" teamContentPath="" />);
    });

    expect(screen.getByText("Loading...")).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// Scroll position memory (lines 194-198)
// ---------------------------------------------------------------------------

describe("Scroll position memory on file switch", () => {
  it("saves scroll position when filePath changes", async () => {
    mockReadFile.mockResolvedValue("content A");

    const { rerender } = render(<MarkdownViewer filePath="/file-a.md" teamContentPath="" />);
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledWith("/file-a.md"));

    mockReadFile.mockResolvedValue("content B");
    rerender(<MarkdownViewer filePath="/file-b.md" teamContentPath="" />);
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledWith("/file-b.md"));

    // No crash, file switched successfully
    expect(screen.getByTestId("react-markdown")).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// File-content cache behavior (Story 32.2 Part 2)
// ---------------------------------------------------------------------------

describe("File-content cache integration", () => {
  it("re-opening the same file at the same refreshKey does NOT call readFile again", async () => {
    mockReadFile.mockResolvedValue("cached content");

    // First render populates the cache.
    const { rerender } = render(
      <MarkdownViewer filePath="/cache-a.md" refreshKey={0} teamContentPath="" />,
    );
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledWith("/cache-a.md"));
    expect(mockReadFile).toHaveBeenCalledTimes(1);

    // Switch to a different file (mount a different path), then back.
    rerender(<MarkdownViewer filePath="/cache-b.md" refreshKey={0} teamContentPath="" />);
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledWith("/cache-b.md"));
    expect(mockReadFile).toHaveBeenCalledTimes(2);

    rerender(<MarkdownViewer filePath="/cache-a.md" refreshKey={0} teamContentPath="" />);
    // Cache hit — no additional readFile call for cache-a.md.
    await new Promise((r) => setTimeout(r, 0));
    expect(mockReadFile).toHaveBeenCalledTimes(2);
  });

  it("cache hit does not show the Loading… spinner", async () => {
    mockReadFile.mockResolvedValue("hello");
    // Preseed the cache at refreshKey=0 so the first render is a hit.
    fileContentCache.set("/preseeded.md", "preseeded content", 0);

    await act(async () => {
      render(<MarkdownViewer filePath="/preseeded.md" refreshKey={0} teamContentPath="" />);
    });

    // No Loading… flash — content renders directly.
    expect(screen.queryByText("Loading...")).toBeNull();
    expect(mockReadFile).not.toHaveBeenCalled();
  });

  it("bumping refreshKey causes a fresh readFile (cache entry is stale)", async () => {
    mockReadFile.mockResolvedValue("v1");

    const { rerender } = render(
      <MarkdownViewer filePath="/reload.md" refreshKey={0} teamContentPath="" />,
    );
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(1));

    // Bump refreshKey — simulates a file-watcher tick.
    mockReadFile.mockResolvedValue("v2");
    rerender(<MarkdownViewer filePath="/reload.md" refreshKey={1} teamContentPath="" />);
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(2));
  });
});

// ---------------------------------------------------------------------------
// F-26: Content-equality guard and per-file refresh awareness
// ---------------------------------------------------------------------------

describe("F-26: content-equality guard (Layer 1)", () => {
  it("skips setContent when readFile returns identical content (refreshKey bump)", async () => {
    mockReadFile.mockResolvedValue("unchanged content");

    const { rerender } = render(
      <MarkdownViewer filePath="/eq.md" refreshKey={0} teamContentPath="" />,
    );
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(1));

    // Bump refreshKey — readFile returns same content
    rerender(<MarkdownViewer filePath="/eq.md" refreshKey={1} teamContentPath="" />);
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(2));

    // Content should still be visible (no flash/blank)
    expect(screen.getByTestId("react-markdown")).toBeInTheDocument();
  });

  it("updates content when readFile returns different content", async () => {
    mockReadFile.mockResolvedValue("v1");

    const { rerender } = render(
      <MarkdownViewer filePath="/change.md" refreshKey={0} teamContentPath="" />,
    );
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(1));

    mockReadFile.mockResolvedValue("v2");
    rerender(<MarkdownViewer filePath="/change.md" refreshKey={1} teamContentPath="" />);
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(2));

    expect(screen.getByTestId("react-markdown")).toBeInTheDocument();
  });
});

describe("F-26: per-burst refresh awareness (Layer 2, Set-aware)", () => {
  it("skips readFile when changedPaths doesn't include filePath", async () => {
    mockReadFile.mockResolvedValue("original");

    // Initial render populates cache
    const { rerender } = render(
      <MarkdownViewer filePath="/my-file.md" refreshKey={0} teamContentPath="" />,
    );
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(1));

    // Bump refreshKey with a changedPaths Set that excludes /my-file.md.
    rerender(
      <MarkdownViewer
        filePath="/my-file.md"
        refreshKey={1}
        changedPaths={new Set(["/other-file.md"])}
        teamContentPath=""
      />,
    );
    await new Promise((r) => setTimeout(r, 0));

    // readFile should NOT have been called a second time (Layer 2 fast-path).
    expect(mockReadFile).toHaveBeenCalledTimes(1);
  });

  it("calls readFile when changedPaths includes filePath", async () => {
    mockReadFile.mockResolvedValue("content");

    const { rerender } = render(
      <MarkdownViewer filePath="/target.md" refreshKey={0} teamContentPath="" />,
    );
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(1));

    mockReadFile.mockResolvedValue("updated");
    rerender(
      <MarkdownViewer
        filePath="/target.md"
        refreshKey={1}
        changedPaths={new Set(["/target.md"])}
        teamContentPath=""
      />,
    );
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(2));
  });

  it("calls readFile when changedPaths is null (broad refresh)", async () => {
    mockReadFile.mockResolvedValue("content");

    const { rerender } = render(
      <MarkdownViewer filePath="/file.md" refreshKey={0} teamContentPath="" />,
    );
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(1));

    rerender(
      <MarkdownViewer filePath="/file.md" refreshKey={1} changedPaths={null} teamContentPath="" />,
    );
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(2));
  });

  // SF-2: blank-tab regression. Fresh mount with a stale-refreshKey cache
  // entry (preloader-seeded, then a bumpFileRefresh for a different file
  // intervenes) used to return from the Layer 2 branch without calling
  // setContent — the user saw an empty prose container. The reconciliation
  // guard inside Layer 2 now sets state from the cache on fresh mount while
  // preserving the F-26 readFile-skip optimization.
  it("fresh mount with a stale-refreshKey cache entry still renders content", async () => {
    fileContentCache.set("/preloaded.md", "preloaded content", 0);

    render(
      <MarkdownViewer
        filePath="/preloaded.md"
        refreshKey={1}
        changedPaths={new Set(["/other.md"])}
        teamContentPath=""
      />,
    );

    await waitFor(() => expect(screen.getByText(/preloaded content/)).toBeInTheDocument());
    expect(mockReadFile).not.toHaveBeenCalled();
  });

  it.each(["targeted", "broad"])(
    "fresh mount renders unchanged disk text after a %s refresh",
    async (refresh) => {
      const path = "/unchanged.md";
      fileContentCache.set(path, "same", 0);
      mockReadFile.mockResolvedValue("same");
      render(
        <MarkdownViewer
          filePath={path}
          refreshKey={1}
          changedPaths={refresh === "targeted" ? new Set([path]) : null}
          teamContentPath=""
        />,
      );
      expect(await screen.findByText("same")).toBeInTheDocument();
      expect(mockReadFile).toHaveBeenCalledWith(path);
      expect(screen.queryByText("Loading...")).not.toBeInTheDocument();
    },
  );

  it("recovers the same content after a read failure cleared the viewer", async () => {
    const path = "/recover.md";
    mockReadFile.mockResolvedValue("same");
    const { rerender } = render(
      <MarkdownViewer filePath={path} refreshKey={0} teamContentPath="" />,
    );
    expect(await screen.findByText("same")).toBeInTheDocument();
    mockReadFile.mockRejectedValueOnce(new Error("Permission denied"));
    rerender(
      <MarkdownViewer filePath={path} refreshKey={1} changedPaths={null} teamContentPath="" />,
    );
    expect(await screen.findByText(/Failed to load file/)).toBeInTheDocument();
    rerender(
      <MarkdownViewer filePath={path} refreshKey={2} changedPaths={null} teamContentPath="" />,
    );
    expect(await screen.findByText("same")).toBeInTheDocument();
    expect(screen.queryByText(/Failed to load file/)).not.toBeInTheDocument();
  });

  it("shows loading until a zero-byte read resolves to a valid empty page", async () => {
    const path = "/empty.md";
    fileContentCache.set(path, "", 0);
    let finish!: (text: string) => void;
    mockReadFile.mockReturnValue(
      new Promise<string>((resolve) => {
        finish = resolve;
      }),
    );
    render(
      <MarkdownViewer filePath={path} refreshKey={1} changedPaths={null} teamContentPath="" />,
    );
    expect(screen.getByText("Loading...")).toBeInTheDocument();
    await act(async () => finish(""));
    expect(screen.getByTestId("react-markdown")).toBeEmptyDOMElement();
    expect(screen.queryByText("Loading...")).not.toBeInTheDocument();
    expect(screen.queryByText(/Failed to load file/)).not.toBeInTheDocument();
  });

  it("SF-3 Bug B: multi-viewer Layer 2 across single Set-shaped burst", async () => {
    // Post-Option-B production behavior: when a debounce window collapses
    // N>1 distinct file paths, App.tsx fires ONE store bump with the full
    // Set. Every viewer sees the same changedPaths and either reads (member)
    // or takes the Layer 2 fast-path (non-member). The fix sidesteps React
    // batching by collapsing N store-set calls into one.
    mockReadFile.mockImplementation((path: string) => Promise.resolve(`content-of-${path}`));

    // Mount viewer A (warms cache for /a.md).
    const { rerender: rerenderA } = render(
      <MarkdownViewer filePath="/a.md" refreshKey={0} teamContentPath="" />,
    );
    await waitFor(() => expect(mockReadFile.mock.calls.some(([p]) => p === "/a.md")).toBe(true));

    // Mount viewer B (warms cache for /b.md).
    const { rerender: rerenderB } = render(
      <MarkdownViewer filePath="/b.md" refreshKey={0} teamContentPath="" />,
    );
    await waitFor(() => expect(mockReadFile.mock.calls.some(([p]) => p === "/b.md")).toBe(true));

    // Mount viewer D, filePath NOT in the burst set — must stay Layer-2-skipped.
    const { rerender: rerenderD } = render(
      <MarkdownViewer filePath="/d.md" refreshKey={0} teamContentPath="" />,
    );
    await waitFor(() => expect(mockReadFile.mock.calls.some(([p]) => p === "/d.md")).toBe(true));

    expect(mockReadFile.mock.calls.filter(([p]) => p === "/a.md").length).toBe(1);
    expect(mockReadFile.mock.calls.filter(([p]) => p === "/b.md").length).toBe(1);
    expect(mockReadFile.mock.calls.filter(([p]) => p === "/d.md").length).toBe(1);

    // Single burst — Set contains 3 paths including /a.md and /b.md but
    // not /d.md. Viewer A is a member → reads. Viewer B is a member → reads.
    // Viewer D is NOT a member → Layer 2 fast-path → no readFile.
    const burst = new Set(["/a.md", "/b.md", "/c.md"]);
    rerenderA(
      <MarkdownViewer filePath="/a.md" refreshKey={1} changedPaths={burst} teamContentPath="" />,
    );
    rerenderB(
      <MarkdownViewer filePath="/b.md" refreshKey={1} changedPaths={burst} teamContentPath="" />,
    );
    rerenderD(
      <MarkdownViewer filePath="/d.md" refreshKey={1} changedPaths={burst} teamContentPath="" />,
    );
    await waitFor(() =>
      expect(mockReadFile.mock.calls.filter(([p]) => p === "/a.md").length).toBe(2),
    );
    await waitFor(() =>
      expect(mockReadFile.mock.calls.filter(([p]) => p === "/b.md").length).toBe(2),
    );

    // Final assertions: members read fresh (1 mount + 1 burst-match = 2);
    // non-member /d.md stays at mount-only (Layer 2 fired); /c.md is in the
    // burst Set but has no mounted viewer, so no read happens for it.
    expect(mockReadFile.mock.calls.filter(([p]) => p === "/a.md").length).toBe(2);
    expect(mockReadFile.mock.calls.filter(([p]) => p === "/b.md").length).toBe(2);
    expect(mockReadFile.mock.calls.filter(([p]) => p === "/d.md").length).toBe(1);
    expect(mockReadFile.mock.calls.filter(([p]) => p === "/c.md").length).toBe(0);
  });
});

// Source view has been removed — viewer always renders markdown
