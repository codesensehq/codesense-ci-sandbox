// SPDX-License-Identifier: MIT
import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, act } from "@testing-library/react";
import { MarkdownViewer } from "./markdown-viewer";
import recoveryPayloads from "@/shared/lib/__test-fixtures__/recovery-payloads.json";
import { bumpKeyForContextFileChange } from "@/shared/lib/build-synthetic-repo";
import { bumpFileRefreshForSourceRepo } from "@/shared/lib/refresh-for-source-repo";
import { flushRefreshDebounce } from "@/app/refresh-debounce";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org } from "@/shared/types/domain";
import { fileContentCache } from "@/shared/lib/file-content-cache";

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

// Mock @/tauri/commands — readFile returns markdown content
const mockReadFile = vi.fn<(path: string) => Promise<string>>();
vi.mock("@/shared/lib/team-key", async () => ({
  parseTeamKey: (await import("@/shared/lib/parse-team-key")).parseTeamKey,
  useActiveTeamKey: () => "_no_team:_none",
  getActiveTeamKey: () => "_no_team:_none",
  buildTeamKey: (orgSlug: string, teamName: string | null) => `${orgSlug}:${teamName ?? "_none"}`,
  NO_TEAM_SENTINEL: "_no_team:_none",
}));

vi.mock("@/tauri/commands", () => ({
  readFile: (...args: unknown[]) => mockReadFile(...(args as [string])),
}));

// Mock viewer store — return configurable settings
const mockStoreValues: Record<string, unknown> = {
  contentWidth: "normal",
  appTheme: "dracula",
  viewerFontSize: "normal",
};
const mockCloseTab = vi.fn();
const mockSetLifecycle = vi.fn();

vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: Object.assign(
    (selector: (s: Record<string, unknown>) => unknown) =>
      selector({
        settings: {
          contentWidth: mockStoreValues.contentWidth,
          appTheme: mockStoreValues.appTheme,
          viewerFontSize: mockStoreValues.viewerFontSize,
        },
      }),
    {
      getState: () => ({
        closeTab: mockCloseTab,
        setOpenFileLifecycle: mockSetLifecycle,
      }),
    },
  ),
}));

// T.6: FileDeletedPlaceholder.onClose reads the active workspace's
// sourceRepoPath via getActiveWorkspaceContext() to build the closeTab
// args. Mock the hook so tests can drive the threading without standing
// up a full viewer-store + org-store fixture.
// T.6: `repoPath` on the context is the synthetic workspace key — what
// closeTab reads to thread into `revealOpenedFile`. The mock uses
// "/repo" as the synthetic-key stand-in so the existing assertion shape
// (closeTab called with "/repo") stays valid.
// SW-FX-38: route `getActiveWorkspaceContext` through a spy so a single test
// can drive a slug-less workspace (repoPath: null). Default keeps the "/repo"
// synthetic-key stand-in so the resolvable-repo assertion above stays valid.
// (Named `mock*` so vitest hoists it into the vi.mock factory.)
const mockGetActiveWorkspaceContext = vi.fn(() => ({
  teamKey: "_no_team:_none",
  workspace: { id: "w1", kind: "root", cwd: "/repo", sourceRepoPath: "/repo", status: "ready" },
  sourceRepoSlug: null,
  repoPath: "/repo" as string | null,
}));

vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  useActiveWorkspaceContext: () => ({
    teamKey: "_no_team:_none",
    workspace: { id: "w1", kind: "root", cwd: "/repo", sourceRepoPath: "/repo", status: "ready" },
    sourceRepoSlug: null,
    repoPath: "/repo",
  }),
  getActiveWorkspaceContext: () => mockGetActiveWorkspaceContext(),
}));

// Mock next-themes
vi.mock("next-themes", () => ({
  useTheme: () => ({ resolvedTheme: "dark" }),
}));

// Mock mermaid
const mockMermaidRender = vi.fn<(id: string, code: string) => Promise<{ svg: string }>>();
vi.mock("mermaid", () => ({
  default: {
    initialize: vi.fn(),
    render: (...args: unknown[]) => mockMermaidRender(...(args as [string, string])),
  },
}));

// Mock react-markdown — renders children as prose div so we can assert content.
// We also capture the `components` prop to test code block rendering (mermaid).
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

// Mock remark/rehype plugins — just pass through
vi.mock("remark-gfm", () => ({ default: () => {} }));
vi.mock("remark-frontmatter", () => ({ default: () => {} }));
vi.mock("rehype-highlight", () => ({ default: () => {} }));

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function setStoreValue(key: string, value: unknown) {
  mockStoreValues[key] = value;
}

beforeEach(() => {
  vi.clearAllMocks();
  mockStoreValues.contentWidth = "normal";
  mockStoreValues.appTheme = "dracula";
  mockStoreValues.viewerFontSize = "normal";
  capturedComponents = {};
  mockReadFile.mockResolvedValue("# Hello World\n\nSome content");
  mockMermaidRender.mockResolvedValue({ svg: "<svg>diagram</svg>" });
});

// ---------------------------------------------------------------------------
// P1-007: Render markdown with syntax highlighting
// ---------------------------------------------------------------------------

describe("@P1 P1-007: Render markdown with syntax highlighting", () => {
  it("renders markdown content as HTML prose", async () => {
    // Given: a markdown file path and content
    mockReadFile.mockResolvedValue("# Hello World\n\nParagraph text here.");

    // When: the component renders
    await act(async () => {
      render(<MarkdownViewer filePath="/test/file.md" teamContentPath="" />);
    });

    // Then: markdown content appears via ReactMarkdown
    await waitFor(() => {
      expect(screen.getByTestId("react-markdown")).toHaveTextContent("# Hello World");
    });
  });

  it("calls readFile with the provided filePath", async () => {
    // Given: a specific file path
    const path = "/docs/readme.md";

    // When: component mounts
    await act(async () => {
      render(<MarkdownViewer filePath={path} teamContentPath="" />);
    });

    // Then: readFile is called with that path
    await waitFor(() => {
      expect(mockReadFile).toHaveBeenCalledWith(path);
    });
  });

  it("shows placeholder when filePath is null", async () => {
    // Given: no file selected

    // When: rendered with null filePath
    await act(async () => {
      render(<MarkdownViewer filePath={null} teamContentPath="" />);
    });

    // Then: shows select-a-file message
    expect(screen.getByText("Select a file to view")).toBeInTheDocument();
  });

  it("wraps content in prose class for typography", async () => {
    // Given: valid markdown
    mockReadFile.mockResolvedValue("Some text");

    // When: rendered
    let container: HTMLElement;
    await act(async () => {
      const result = render(<MarkdownViewer filePath="/test.md" teamContentPath="" />);
      container = result.container;
    });

    // Then: prose class is applied
    await waitFor(() => {
      const proseEl = container!.querySelector(".prose");
      expect(proseEl).not.toBeNull();
    });
  });
});

// ---------------------------------------------------------------------------
// P1-008: Mermaid diagram render
// ---------------------------------------------------------------------------

describe("@P1 P1-008: Mermaid diagram render", () => {
  it("delegates mermaid code blocks to MermaidDiagram component", async () => {
    // Given: markdown with a mermaid code block
    mockReadFile.mockResolvedValue("```mermaid\ngraph TD; A-->B\n```");

    // When: component renders and ReactMarkdown captures components
    await act(async () => {
      render(<MarkdownViewer filePath="/test.md" teamContentPath="" />);
    });

    // Then: the code component handler recognizes language-mermaid
    await waitFor(() => {
      expect(capturedComponents.code).toBeDefined();
    });

    // When: we invoke the code component with mermaid className
    const CodeComponent = capturedComponents.code as (props: {
      className: string;
      children: string;
    }) => React.ReactElement;

    let mermaidResult: HTMLElement;
    await act(async () => {
      const result = render(
        <CodeComponent className="language-mermaid">{"graph TD; A-->B"}</CodeComponent>,
      );
      mermaidResult = result.container;
    });

    // Then: it renders a div (MermaidDiagram container), not a <code> element
    await waitFor(() => {
      // MermaidDiagram renders a div with class my-4
      const diagramDiv = mermaidResult!.querySelector(".my-4");
      expect(diagramDiv).not.toBeNull();
    });
  });

  it("renders non-mermaid code blocks as normal <code> elements", async () => {
    // Given: markdown loaded
    mockReadFile.mockResolvedValue("```js\nconsole.log('hi')\n```");

    // When: component renders
    await act(async () => {
      render(<MarkdownViewer filePath="/test.md" teamContentPath="" />);
    });

    await waitFor(() => {
      expect(capturedComponents.code).toBeDefined();
    });

    // When: code component invoked with non-mermaid class
    const CodeComponent = capturedComponents.code as (props: {
      className: string;
      children: string;
    }) => React.ReactElement;

    const { container } = render(
      <CodeComponent className="language-javascript">{"console.log('hi')"}</CodeComponent>,
    );

    // Then: it renders a <code> element
    expect(container.querySelector("code")).not.toBeNull();
    expect(container.querySelector(".my-4")).toBeNull();
  });

  it("mermaid render failure shows fallback text", async () => {
    // Given: mermaid.render rejects
    mockMermaidRender.mockRejectedValue(new Error("parse error"));
    mockReadFile.mockResolvedValue("```mermaid\ninvalid\n```");

    // When: component renders and code component creates MermaidDiagram
    await act(async () => {
      render(<MarkdownViewer filePath="/test.md" teamContentPath="" />);
    });

    await waitFor(() => {
      expect(capturedComponents.code).toBeDefined();
    });

    const CodeComponent = capturedComponents.code as (props: {
      className: string;
      children: string;
    }) => React.ReactElement;

    await act(async () => {
      render(<CodeComponent className="language-mermaid">{"invalid diagram"}</CodeComponent>);
    });

    // Then: mermaid.render was called (and will fail, showing fallback text)
    await waitFor(() => {
      expect(mockMermaidRender).toHaveBeenCalled();
    });
  });
});

// ---------------------------------------------------------------------------
// P1-009: Theme switching (5 viewer themes)
// ---------------------------------------------------------------------------

describe("@P1 P1-009: Theme switching — viewer applies rounded-none for non-default themes", () => {
  const themes = ["dracula", "github", "nord", "catppuccin", "confluence"] as const;

  for (const theme of themes) {
    it(`applies rounded-none for ${theme} theme (theme class is on parent wrapper)`, async () => {
      // Given: appTheme is set
      setStoreValue("appTheme", theme);

      // When: component renders
      let container: HTMLElement;
      await act(async () => {
        const result = render(<MarkdownViewer filePath="/test.md" teamContentPath="" />);
        container = result.container;
      });

      // Then: the scroll container has rounded-none (theme class is on parent, not here)
      await waitFor(() => {
        const scrollEl = container!.querySelector(".rounded-none");
        expect(scrollEl).not.toBeNull();
      });
    });
  }

  it("does not apply rounded-none for 'default' theme", async () => {
    // Given: default theme
    setStoreValue("appTheme", "codesense");

    // When: component renders
    let container: HTMLElement;
    await act(async () => {
      const result = render(<MarkdownViewer filePath="/test.md" teamContentPath="" />);
      container = result.container;
    });

    // Then: no rounded-none class present
    await waitFor(() => {
      const el = container!.querySelector(".rounded-none");
      expect(el).toBeNull();
    });
  });
});

// ---------------------------------------------------------------------------
// SR-9: viewer reload-storm dedupe — content-version gating
//
// The store change (contentRefreshKeys, counter-only bumps skip it) means a
// ContentLayerEvent materialize storm HOLDS the viewer's `refreshKey` prop steady
// while it fires ~1×/sec. These tests prove the viewer's consequence: a held key
// (storm) triggers NO re-read; an advanced key + matching changed-path (genuine
// change) DOES. Together with the store test, this is the AC-1/AC-3 end-to-end proof.
// ---------------------------------------------------------------------------

describe("@P1 SR-9: viewer reload-storm dedupe", () => {
  it("AC-1: a held refreshKey (counter-only storm) triggers ZERO re-reads", async () => {
    const path = "/sr9/open-unchanged.md";
    fileContentCache.invalidate(path);
    mockReadFile.mockResolvedValue("# Open file\n\nunchanged bytes");

    let rerender!: (ui: React.ReactElement) => void;
    await act(async () => {
      const r = render(
        <MarkdownViewer filePath={path} refreshKey={5} changedPaths={null} teamContentPath="" />,
      );
      rerender = r.rerender;
    });
    await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(1));

    // Simulate N ContentLayerEvent storm waves: `contentRefreshKeys` is NOT bumped,
    // so the viewer re-renders with the SAME key + broad null changedPaths.
    for (let i = 0; i < 5; i++) {
      await act(async () => {
        rerender(
          <MarkdownViewer filePath={path} refreshKey={5} changedPaths={null} teamContentPath="" />,
        );
      });
    }

    // No additional read — the storm is a no-op for the open, unchanged file.
    expect(mockReadFile).toHaveBeenCalledTimes(1);
  });

  it.each(["file", "directory"])(
    "AC-3: a %s change re-reads and re-renders markdown",
    async (shape) => {
      const path = "/sr9/genuinely-changed.md";
      fileContentCache.invalidate(path);
      mockReadFile.mockResolvedValue("# Version one");

      let rerender!: (ui: React.ReactElement) => void;
      await act(async () => {
        const r = render(
          <MarkdownViewer filePath={path} refreshKey={1} changedPaths={null} teamContentPath="" />,
        );
        rerender = r.rerender;
      });
      await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(1));

      // A genuine content-affecting bump advances contentRefreshKeys AND carries the
      // path in changedPaths.
      mockReadFile.mockResolvedValue("# Version two");
      await act(async () => {
        rerender(
          <MarkdownViewer
            filePath={path}
            refreshKey={2}
            changedPaths={new Set([shape === "file" ? path : "/sr9"])}
            teamContentPath=""
          />,
        );
      });

      await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(2));
      await waitFor(() => {
        expect(screen.getByTestId("react-markdown")).toHaveTextContent("# Version two");
      });
    },
  );
});

// Source view mode has been removed — viewer always renders markdown

// ---------------------------------------------------------------------------
// P2-013: File deleted error handling
// ---------------------------------------------------------------------------

describe("@P2 P2-013: File deleted error handling — shows error with close button", () => {
  it("shows file-deleted UI when readFile fails with 'No such file'", async () => {
    // Given: readFile rejects with file-not-found error
    mockReadFile.mockRejectedValue(new Error("No such file or directory"));

    // When: component renders
    await act(async () => {
      render(<MarkdownViewer filePath="/repo/deleted-file.md" teamContentPath="" />);
    });

    // Then: shows deleted message with file name
    await waitFor(() => {
      expect(screen.getByText("This file was removed from disk")).toBeInTheDocument();
      expect(screen.getByText("deleted-file.md")).toBeInTheDocument();
    });
  });

  it("shows file-deleted UI when readFile fails with 'os error 2'", async () => {
    // Given: readFile rejects with os error 2
    mockReadFile.mockRejectedValue(new Error("os error 2"));

    // When: component renders
    await act(async () => {
      render(<MarkdownViewer filePath="/repo/gone.md" teamContentPath="" />);
    });

    // Then: shows deleted message
    await waitFor(() => {
      expect(screen.getByText("This file was removed from disk")).toBeInTheDocument();
    });
  });

  it("renders Close Tab button that calls closeTab with active workspace's repo", async () => {
    // Given: file is deleted
    mockReadFile.mockRejectedValue(new Error("No such file or directory"));

    // When: component renders
    await act(async () => {
      render(<MarkdownViewer filePath="/repo/deleted.md" teamContentPath="" />);
    });

    // Then: Close Tab button is present
    const closeBtn = await waitFor(() => screen.getByText("Close Tab"));
    expect(closeBtn).toBeInTheDocument();

    // When: button is clicked
    await act(async () => {
      closeBtn.click();
    });

    // Then: closeTab is called with (teamKey, repoPath, filePath).
    // T.6: repoPath comes from getActiveWorkspaceContext().workspace.sourceRepoPath
    // ("/repo" from the mock above), teamKey from getActiveTeamKey()
    // ("_no_team:_none" from the team-key mock).
    expect(mockCloseTab).toHaveBeenCalledWith("_no_team:_none", "/repo", "/repo/deleted.md", {
      user: true,
    });
  });

  it("closes the tab in a slug-less workspace (SW-FX-38: repoPath null → '')", async () => {
    // Given: file is deleted AND the active workspace is slug-less
    // (kind:"epic", or none focused) → getActiveWorkspaceContext() yields
    // repoPath: null. The Close Tab button must STILL close the tab —
    // `closeTab` keys off (teamKey, filePath); repoPath is best-effort for the
    // reveal (safe on ""). RED on baseline (`repoPath && filePath` blocked it).
    mockReadFile.mockRejectedValue(new Error("No such file or directory"));
    mockGetActiveWorkspaceContext.mockReturnValueOnce({
      teamKey: "_no_team:_none",
      workspace: { id: "e1", kind: "root", cwd: "/epic", sourceRepoPath: "/epic", status: "ready" },
      sourceRepoSlug: null,
      repoPath: null,
    });

    await act(async () => {
      render(<MarkdownViewer filePath="/repo/deleted.md" teamContentPath="" />);
    });

    const closeBtn = await waitFor(() => screen.getByText("Close Tab"));
    await act(async () => {
      closeBtn.click();
    });

    // repoPath falls back to "" since the slug-less workspace yields null.
    expect(mockCloseTab).toHaveBeenCalledWith("_no_team:_none", "", "/repo/deleted.md", {
      user: true,
    });
  });

  it("shows generic error for non-deleted-file errors", async () => {
    // Given: readFile rejects with a different error
    mockReadFile.mockRejectedValue(new Error("Permission denied"));

    // When: component renders
    await act(async () => {
      render(<MarkdownViewer filePath="/repo/forbidden.md" teamContentPath="" />);
    });

    // Then: shows generic error, not the deleted-file UI
    await waitFor(() => {
      expect(screen.getByText(/Failed to load file/)).toBeInTheDocument();
      expect(screen.queryByText("This file was removed from disk")).not.toBeInTheDocument();
    });
  });

  // SW-AU-85A-iii: the viewer publishes its OWN read outcome so the editor tab's subject
  // badge can render an honest lifecycle without any per-tab filesystem probe.
  it("publishes 'deleted' lifecycle when the open file is missing on disk", async () => {
    mockReadFile.mockRejectedValue(new Error("No such file or directory (os error 2)"));

    await act(async () => {
      render(<MarkdownViewer filePath="/repo/deleted-file.md" teamContentPath="" />);
    });

    await waitFor(() =>
      expect(mockSetLifecycle).toHaveBeenCalledWith("/repo/deleted-file.md", "deleted"),
    );
  });

  it("publishes 'temporarily_unavailable' for a non-missing read failure", async () => {
    mockReadFile.mockRejectedValue(new Error("Permission denied"));

    await act(async () => {
      render(<MarkdownViewer filePath="/repo/forbidden.md" teamContentPath="" />);
    });

    await waitFor(() =>
      expect(mockSetLifecycle).toHaveBeenCalledWith(
        "/repo/forbidden.md",
        "temporarily_unavailable",
      ),
    );
  });

  it("clears the lifecycle entry on a successful read (file present ≡ active)", async () => {
    mockReadFile.mockResolvedValue("# Present\n\nbody");

    await act(async () => {
      render(<MarkdownViewer filePath="/repo/present.md" teamContentPath="" />);
    });

    await waitFor(() => expect(mockSetLifecycle).toHaveBeenCalledWith("/repo/present.md", null));
  });

  it("clears its file's lifecycle on unmount (the viewer OWNS the entry, bounding the slice)", async () => {
    mockReadFile.mockRejectedValue(new Error("No such file or directory (os error 2)"));

    const { unmount } = await act(async () =>
      render(<MarkdownViewer filePath="/repo/gone.md" teamContentPath="" />),
    );
    await waitFor(() => expect(mockSetLifecycle).toHaveBeenCalledWith("/repo/gone.md", "deleted"));

    mockSetLifecycle.mockClear();
    act(() => unmount());
    // Unmount drops the entry so a closed/switched-away file leaves nothing behind.
    expect(mockSetLifecycle).toHaveBeenCalledWith("/repo/gone.md", null);
  });

  it("does NOT publish a superseded read's outcome (cancellation guard on the read effect)", async () => {
    // A read that is still in flight when the effect tears down must not write content,
    // error, OR lifecycle — otherwise a slow deleted-catch could clobber a fresh restore.
    let rejectRead: (e: unknown) => void = () => {};
    mockReadFile.mockReturnValue(
      new Promise((_resolve, reject) => {
        rejectRead = reject;
      }),
    );

    const { unmount } = render(<MarkdownViewer filePath="/repo/racing.md" teamContentPath="" />);
    // Tear the effect down BEFORE the read settles (unmount → cleanup marks it superseded).
    act(() => unmount());
    mockSetLifecycle.mockClear();

    // Now the stale read rejects late. Its handler must be a no-op.
    await act(async () => {
      rejectRead(new Error("No such file or directory (os error 2)"));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(mockSetLifecycle).not.toHaveBeenCalledWith("/repo/racing.md", "deleted");
  });
});

// This contract is also asserted against actual registered Rust recovery emissions.
describe("registered recovery payload through routing and the open viewer", () => {
  it.each(["inherited", "private", "private-deleted"] as const)(
    "rereads an open %s physical file",
    async (kind) => {
      const deleting = kind === "private-deleted";
      const payload = recoveryPayloads[kind === "inherited" ? "inherited" : "private"];
      const path = `${payload.file_path}/docs/a.md`;
      const key =
        kind === "inherited" ? "team:acme/app:backend" : recoveryPayloads.private.repo_path;
      useOrgStore.setState({
        orgs: [
          {
            slug: "acme/app",
            display_name: "App",
            org_root: "/fixture/org",
            created_at: "",
            remote_url: null,
            config_flags: {},
            people: [],
            teams: [
              { name: "backend", parent_team: "platform", source_repos: [] },
              { name: "platform", source_repos: [] },
            ],
          } satisfies Org,
        ],
        workspaceRepos: {},
      });
      useFileRefreshStore.setState({
        fileRefreshKeys: {},
        contentRefreshKeys: {},
        lastChangedContextFile: {},
      });
      fileContentCache.invalidate(path);
      function OpenViewer() {
        const refreshKey = useFileRefreshStore((s) => s.contentRefreshKeys[key] ?? 0);
        const changedPaths = useFileRefreshStore((s) => s.lastChangedContextFile[key]);
        return (
          <MarkdownViewer
            filePath={path}
            refreshKey={refreshKey}
            changedPaths={changedPaths}
            teamContentPath=""
          />
        );
      }
      mockReadFile.mockResolvedValue("# Before recovery");
      render(<OpenViewer />);
      await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(1));
      if (deleting) mockReadFile.mockRejectedValue(new Error("No such file or directory"));
      else mockReadFile.mockResolvedValue("# After recovery");
      act(() => {
        const routed = bumpKeyForContextFileChange(payload);
        expect(routed).toBe(key);
        flushRefreshDebounce(routed!, new Set([payload.file_path]), bumpFileRefreshForSourceRepo, {
          warn: () => {},
        });
      });
      await waitFor(() => expect(mockReadFile).toHaveBeenCalledTimes(2));
      if (deleting) {
        await waitFor(() =>
          expect(screen.getByText("This file was removed from disk")).toBeInTheDocument(),
        );
        expect(screen.getByText("a.md")).toBeInTheDocument();
      } else {
        expect(screen.getByTestId("react-markdown")).toHaveTextContent("# After recovery");
      }
      expect(useFileRefreshStore.getState().contentRefreshKeys[key]).toBe(1);
      expect(
        useFileRefreshStore.getState().contentRefreshKeys["team:acme/app:platform"],
      ).toBeUndefined();
    },
  );
});
