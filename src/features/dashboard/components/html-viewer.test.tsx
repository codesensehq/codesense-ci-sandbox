// SPDX-License-Identifier: MIT
const { nativeDispatch, nativeSettle } = vi.hoisted(() => ({
  nativeDispatch: vi.fn(async (_caller: unknown, verb: string) =>
    ["setStatus", "draft", "routeToContext", "getDependencyStatus", "openWorkspace"].includes(verb)
      ? { status: "admit", callId: "call" }
      : { status: "deny", reason: "denied: not allowlisted" },
  ),
  nativeSettle: vi.fn(async () => undefined),
}));
vi.mock("../lib/verb-dispatch", () => ({
  verbDispatch: nativeDispatch,
  verbSettle: nativeSettle,
  verbAnswer: vi.fn(),
}));
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));
import { FrameMessageEvent } from "../../../../tests/support/fixtures/frame";
import { runRenderedDocumentRequest } from "@/shared/hooks/use-rendered-document-bridge";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { StrictMode } from "react";
import { render, screen, waitFor, cleanup, fireEvent } from "@testing-library/react";
import { HtmlViewer, NETWORK_PROBLEM_GRACE_MS } from "./html-viewer";

// All mocks referenced inside `vi.mock` factories are created via `vi.hoisted`
// so they're initialized before the (hoisted) factories run.
const {
  mockGetHttpApiPort,
  mockGetDependencyStatus,
  mockWriteContentFile,
  mockToastSuccess,
  mockToastError,
  mockCloseTab,
  mockToggleOrgSwitcher,
  mockReadFile,
  mockDispatchMenuAction,
  mockApplyMetadataEdit,
  mockRouteFileToContext,
  mockCreateFile,
  mockLoggerInfo,
  mockLoggerWarn,
} = vi.hoisted(() => ({
  mockGetHttpApiPort: vi.fn(),
  mockGetDependencyStatus: vi.fn(),
  // SW-SD-1: the snapshot archive sink (already-exposed context writer).
  mockWriteContentFile: vi.fn(),
  mockToastSuccess: vi.fn(),
  mockToastError: vi.fn(),
  mockCloseTab: vi.fn(),
  // SLW-FX-22: switch-org routes through the global ui-store toggle.
  mockToggleOrgSwitcher: vi.fn(),
  // The action command wrappers + audit.
  mockReadFile: vi.fn(),
  mockDispatchMenuAction: vi.fn(),
  mockApplyMetadataEdit: vi.fn(),
  mockRouteFileToContext: vi.fn(),
  mockCreateFile: vi.fn(),
  mockLoggerInfo: vi.fn(),
  mockLoggerWarn: vi.fn(),
}));

vi.mock("@/app/menu-action-dispatch", () => ({
  dispatchMenuAction: (...args: unknown[]) => mockDispatchMenuAction(...args),
  getMenuActionGate: () => ({
    settingsPanelOpen: false,
    hasActiveWorkspace: shortcutWorkspace !== null,
    hasTeam: true,
    rowPopupFocused: false,
  }),
}));
vi.mock("@/shared/hooks/use-active-workspace-context", async (original) => ({
  ...(await original<typeof import("@/shared/hooks/use-active-workspace-context")>()),
  getActiveWorkspaceContext: () => ({ teamKey: shortcutTeamKey, workspace: shortcutWorkspace }),
}));
const { shortcutActions } = vi.hoisted(() => ({
  shortcutActions: [
    {
      id: "viewer.toggle-agent-inbox",
      defaultKeystroke: "CmdOrCtrl+I",
      scope: "workspace",
      macCmdOnly: true,
    },
    {
      id: "viewer.toggle-inbox-rollup",
      defaultKeystroke: "CmdOrCtrl+Shift+I",
      scope: "team",
      macCmdOnly: true,
    },
    { id: "viewer.toggle-nav", defaultKeystroke: "CmdOrCtrl+B", scope: "team" },
    { id: "viewer.toggle-terminal", defaultKeystroke: "CmdOrCtrl+\\", scope: "team" },
    {
      id: "viewer.split-terminal",
      defaultKeystroke: "CmdOrCtrl+D",
      scope: "workspace",
      macCmdOnly: true,
    },
    { id: "viewer.focus-pane-right", defaultKeystroke: "CmdOrCtrl+Alt+ArrowRight", scope: "team" },
    { id: "nav.switch-org", defaultKeystroke: "CmdOrCtrl+Shift+G", scope: "global" },
    { id: "nav.quick-open", defaultKeystroke: "CmdOrCtrl+P", scope: "global", macCmdOnly: true },
    { id: "nav.context-back", defaultKeystroke: "CmdOrCtrl+[", scope: "global", macCmdOnly: true },
    {
      id: "nav.context-forward",
      defaultKeystroke: "CmdOrCtrl+]",
      scope: "global",
      macCmdOnly: true,
    },
  ],
}));
let shortcutWorkspace: object | null = {};
let shortcutTeamKey = "_no_team:_none";
vi.mock("@/shared/lib/shortcuts/registry", () => ({
  SHORTCUT_ACTIONS: shortcutActions,
  allShortcutActions: () => shortcutActions,
  resolveBindings: (overrides: Record<string, string>) =>
    new Map(
      shortcutActions.map((a) => [a.id, { keystroke: overrides[a.id] ?? a.defaultKeystroke }]),
    ),
  dashboardCommandActionId: (dashboard: string, id: string) => `${dashboard}/${id}`,
  setDynamicShortcutActions: () => {},
}));
vi.mock("@/shared/stores/shortcuts-store", () => ({
  useShortcutsStore: { getState: () => ({ shortcuts: shortcutOverrides }) },
}));
let shortcutOverrides: Record<string, string> = {};

vi.mock("@/tauri/commands", async () => ({
  ...(await import("../../../../tests/support/fixtures/frame")).frameCommands(),
  bridgeDocContext: async () => null,
  admitRenderedDocument: vi.fn().mockResolvedValue(undefined),
  getHttpApiPort: () => mockGetHttpApiPort(),
  // SW-LD-1: html-bridge maps `getDependencyStatus` — a real allowlist entry.
  getDependencyStatus: () => mockGetDependencyStatus(),
  // SW-SD-1: reuse the existing context writer as the snapshot sink.
  writeContentFile: (path: string, content: string) => mockWriteContentFile(path, content),
  readFile: (path: string) => mockReadFile(path),
  applyMetadataEdit: (path: string, edits: unknown) => mockApplyMetadataEdit(path, edits),
  routeFileToContext: (...args: unknown[]) => mockRouteFileToContext(...args),
  createFile: (path: string, content: string) => mockCreateFile(path, content),
}));

// SW-DX-6: mock the app logger so the audit line is assertable + quiet.
vi.mock("@/shared/lib/logger", () => ({
  logger: {
    info: (...args: unknown[]) => mockLoggerInfo(...args),
    warn: (...args: unknown[]) => mockLoggerWarn(...args),
    error: vi.fn(),
    debug: vi.fn(),
    trace: vi.fn(),
  },
}));

// SW-SD-1: Sonner toast — assert success/failure surfacing without a real DOM toast.
vi.mock("sonner", () => ({
  toast: {
    success: (...args: unknown[]) => mockToastSuccess(...args),
    error: (...args: unknown[]) => mockToastError(...args),
  },
}));

// Mock viewer store — callable hook (selector form) plus `getState`.
vi.mock("@/shared/stores/viewer-store", () => {
  const state = {
    selectedRepoPath: "/repo",
    closeTab: mockCloseTab,
    settings: {},
  };
  const useViewerStore = Object.assign(
    (selector: (s: typeof state) => unknown) => selector(state),
    { subscribe: () => () => {}, getState: () => state },
  );
  return {
    useViewerStore,
    selectTerminalTabIndex: () => ({}),
    selectActiveTerminalTab: () => undefined,
    selectFocusedSession: () => undefined,
  };
});

// SLW-FX-22: the ui-store hosts `toggleOrgSwitcher` — the exact primitive the
// global Cmd+Shift+G shortcut fires. html-viewer routes the forwarded chord to
// it via `getState()`, so we stub the store with a spy.
vi.mock("@/shared/stores/ui-store", () => {
  const state = { toggleOrgSwitcher: mockToggleOrgSwitcher };
  const useUiStore = Object.assign((selector: (s: typeof state) => unknown) => selector(state), {
    getState: () => state,
  });
  return { useUiStore };
});

// Mock @tauri-apps/plugin-opener
const mockOpenUrl = vi.fn().mockResolvedValue(undefined);
vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: (...args: unknown[]) => mockOpenUrl(...args),
}));

// SW-DX-7: stub the live controller registration so these bridge tests run with
// NO dashboard-launch controller mounted (the openWorkspace/runPrompt
// forward-to-controller behavior + the real launch are covered by
// dashboard-launch.test.ts, html-bridge.test.ts and the no-grants seam test).
vi.mock("./dashboard-launch-controller", () => ({
  useRegisterDashboardLaunchController: () => {},
}));

// Mock fetch for HEAD pre-check
const mockFetch = vi.fn();
globalThis.fetch = mockFetch;

beforeEach(() => {
  vi.clearAllMocks();
  mockFetch.mockResolvedValue({ status: 200 });
  mockGetDependencyStatus.mockResolvedValue({ ok: true });
  mockWriteContentFile.mockResolvedValue(undefined);
  mockReadFile.mockResolvedValue("<html><head></head><body></body></html>");
  mockApplyMetadataEdit.mockResolvedValue(undefined);
  localStorage.clear();
});

afterEach(() => {
  cleanup();
});

describe("HtmlViewer", () => {
  it.each([
    [undefined, "page.html"],
    ["Triage inbox", "Triage inbox"],
  ] as const)("labels the frame with %s or its filename", async (title, expected) => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(<HtmlViewer filePath="/repo/page.html" teamContentPath="/repo" frameTitle={title} />);
    await waitFor(() => expect(screen.getByTitle(expected)).toBeInstanceOf(HTMLIFrameElement));
  });
  it("shows loading state while port resolves", () => {
    // Never resolve the promise
    mockGetHttpApiPort.mockReturnValue(new Promise(() => {}));
    render(<HtmlViewer filePath="/repo/page.html" teamContentPath="/repo" />);
    expect(screen.getByText("Loading...")).toBeInTheDocument();
  });

  it("shows error when HTTP server is disabled", async () => {
    mockGetHttpApiPort.mockResolvedValue(null);
    render(<HtmlViewer filePath="/repo/page.html" teamContentPath="/repo" />);
    await waitFor(() => {
      expect(screen.getByText("HTTP API server is not available")).toBeInTheDocument();
    });
  });

  it("renders iframe with correct src URL", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(<HtmlViewer filePath="/repo/docs/page.html" refreshKey={5} teamContentPath="/repo" />);
    await waitFor(() => {
      const iframe = document.querySelector("iframe");
      expect(iframe).toBeTruthy();
      const src = iframe!.getAttribute("src")!;
      expect(src).toContain("http://f-test.localhost:9123/viewer/");
      expect(src).toContain("docs/page.html");
      expect(src).toContain("v=5");
      expect(JSON.parse(new URL(src).searchParams.get("theme")!)).toEqual(
        expect.objectContaining({
          scheme: expect.stringMatching(/dark|light/),
          tokens: expect.objectContaining({ "--cs-bg": expect.any(String) }),
        }),
      );
    });
  });

  it("grants full sandbox capabilities to every live doc", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(<HtmlViewer filePath="/repo/page.html" teamContentPath="/repo" />);
    await waitFor(() => {
      const iframe = document.querySelector("iframe");
      expect(iframe).toBeTruthy();
      const sandbox = iframe!.getAttribute("sandbox")!;
      expect(sandbox).toContain("allow-scripts");
      expect(sandbox).toContain("allow-same-origin");
      expect(sandbox).toContain("allow-forms");
    });
  });

  it("keeps credentials out of every live document URL", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(<HtmlViewer filePath="/repo/dashboard.html" teamContentPath="/repo" />);
    await waitFor(() => {
      const src = document.querySelector("iframe")?.getAttribute("src");
      expect(src).not.toContain("cap=");
    });
  });

  it("does NOT append the capability for a baseline/diff render", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(
      <HtmlViewer
        filePath="/repo/dashboard.html"
        teamContentPath="/repo"
        baselineSource={{ orgRoot: "/repo", relPath: "dashboard.html" }}
      />,
    );
    await waitFor(() => {
      const src = document.querySelector("iframe")?.getAttribute("src");
      expect(src).toBeTruthy();
      expect(src).not.toContain("cap=");
      expect(src).toContain("http://f-test.localhost:9123/viewer-baseline/");
    });
  });

  it("renders a baseline/diff render in the ISOLATED sandbox", async () => {
    // A historical/other-tree "before" blob is shown for comparison, never run as
    // a dashboard, so it gets no same-origin and no bridge.
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(
      <HtmlViewer
        filePath="/repo/page.html"
        teamContentPath="/repo"
        baselineSource={{ orgRoot: "/repo", relPath: "page.html" }}
      />,
    );
    await waitFor(() => {
      const iframe = document.querySelector("iframe");
      expect(iframe).toBeTruthy();
      expect(iframe!.getAttribute("sandbox")).toBe("allow-scripts");
    });
  });

  it("REMOUNTS the iframe when the baseline view toggles off so the frame reloads first-class", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    const baseline = { orgRoot: "/repo", relPath: "page.html" };
    const { rerender } = render(
      <HtmlViewer filePath="/repo/page.html" teamContentPath="/repo" baselineSource={baseline} />,
    );
    const before = await waitFor(() => {
      const el = document.querySelector("iframe");
      expect(el).toBeTruthy();
      return el!;
    });
    expect(before.getAttribute("sandbox")).toBe("allow-scripts");

    rerender(<HtmlViewer filePath="/repo/page.html" teamContentPath="/repo" />);

    const after = await waitFor(() => {
      const el = document.querySelector("iframe");
      expect(el!.getAttribute("sandbox")).toContain("allow-same-origin");
      return el!;
    });
    // A NEW element instance: the sandbox is fixed at load time, so the frame
    // must remount rather than swap its sandbox attribute in place.
    expect(after).not.toBe(before);
  });

  it("renders no trust affordance", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(<HtmlViewer filePath="/repo/page.html" teamContentPath="/repo" />);
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123/",
      ),
    );
    expect(screen.queryByText(/untrusted|trust/i)).toBeNull();
  });

  it("shows FileDeletedPlaceholder on 404", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    mockFetch.mockResolvedValue({ status: 404 });
    render(<HtmlViewer filePath="/repo/deleted.html" teamContentPath="/repo" />);
    await waitFor(() => {
      expect(screen.getByText("This file was removed from disk")).toBeInTheDocument();
    });
    expect(screen.getByText("deleted.html")).toBeInTheDocument();
  });

  it("closes the tab on Close Tab click in a slug-less workspace (SW-FX-38)", async () => {
    // 404 → FileDeletedPlaceholder. No org is active in the test, so the real
    // getActiveWorkspaceContext() resolves teamKey to the sentinel and
    // repoPath to null — the slug-less/no-workspace state. The Close Tab button
    // must STILL close the tab: `closeTab` keys off (teamKey, filePath) and
    // repoPath falls back to "". RED on baseline (`repoPath && filePath`
    // blocked the close).
    mockGetHttpApiPort.mockResolvedValue(9123);
    mockFetch.mockResolvedValue({ status: 404 });
    render(<HtmlViewer filePath="/repo/deleted.html" teamContentPath="/repo" />);

    const closeBtn = await waitFor(() => screen.getByText("Close Tab"));
    fireEvent.click(closeBtn);

    expect(mockCloseTab).toHaveBeenCalledWith("_no_team:_none", "", "/repo/deleted.html", {
      user: true,
    });
  });

  it("shows placeholder when filePath is null", () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(<HtmlViewer filePath={null} teamContentPath="/repo" />);
    expect(screen.getByText("Select a file to view")).toBeInTheDocument();
  });
});

// ---------------------------------------------------------------------------
// postMessage listener (R.3)
// ---------------------------------------------------------------------------

/**
 * Dispatch a MessageEvent whose `source` matches the rendered iframe's
 * `contentWindow`.  happy-dom doesn't populate `contentWindow` on
 * iframes, so we use `Object.defineProperty` to simulate it.
 */
function dispatchIframeMessage(data: Record<string, unknown>) {
  const iframe = document.querySelector("iframe");
  // Simulate contentWindow matching event.source
  const fakeWindow = {} as Window;
  if (iframe) {
    Object.defineProperty(iframe, "contentWindow", {
      value: fakeWindow,
      writable: true,
      configurable: true,
    });
  }
  const event = new FrameMessageEvent("message", { data, source: fakeWindow });
  window.dispatchEvent(event);
}

function dispatchForeignMessage(data: Record<string, unknown>) {
  // Source is a different window — should be ignored by the listener
  const event = new FrameMessageEvent("message", {
    data,
    source: {} as Window,
  });
  window.dispatchEvent(event);
}

describe("HtmlViewer postMessage listener", () => {
  const filePath = "/repo/_bmad-output/docs/dashboard.html";

  async function renderWithNav() {
    mockGetHttpApiPort.mockResolvedValue(9123);
    const onNavigate = vi.fn();
    render(<HtmlViewer filePath={filePath} teamContentPath="/repo" onNavigate={onNavigate} />);
    await waitFor(() => {
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123",
      );
    });
    return onNavigate;
  }

  it("codesense:navigate calls onNavigate with resolved path", async () => {
    const onNavigate = await renderWithNav();
    dispatchIframeMessage({
      type: "codesense:navigate",
      href: "../readme.md",
      metaKey: false,
      ctrlKey: false,
    });
    expect(onNavigate).toHaveBeenCalledWith("/repo/_bmad-output/readme.md", {});
  });

  it("a pane's relative link resolves from its team's content, not its panes/ dir", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    const onNavigate = vi.fn();
    render(
      <HtmlViewer
        filePath="/org/backend/panes/board.html"
        teamContentPath="/org/backend/content"
        pane={{
          contentRoot: "/org/backend/content",
          panesDir: "/org/backend/panes",
          fileName: "board.html",
        }}
        onNavigate={onNavigate}
      />,
    );
    await waitFor(() => {
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain("/pane/");
    });
    dispatchIframeMessage({
      type: "codesense:navigate",
      href: "stories/x.md",
      metaKey: false,
      ctrlKey: false,
    });
    expect(onNavigate).toHaveBeenCalledWith("/org/backend/content/stories/x.md", {});
  });

  it("normalizes relative paths with ..", async () => {
    const onNavigate = await renderWithNav();
    dispatchIframeMessage({
      type: "codesense:navigate",
      href: "../../overview.md",
      metaKey: false,
      ctrlKey: false,
    });
    expect(onNavigate).toHaveBeenCalledWith("/repo/overview.md", {});
  });

  it("forwards fragment as options.hash", async () => {
    const onNavigate = await renderWithNav();
    dispatchIframeMessage({
      type: "codesense:navigate",
      href: "guide.md#setup",
      metaKey: false,
      ctrlKey: false,
    });
    expect(onNavigate).toHaveBeenCalledWith("/repo/_bmad-output/docs/guide.md", {
      hash: "#setup",
    });
  });

  it("metaKey=true sets openInNewTab", async () => {
    const onNavigate = await renderWithNav();
    dispatchIframeMessage({
      type: "codesense:navigate",
      href: "other.html",
      metaKey: true,
      ctrlKey: false,
    });
    expect(onNavigate).toHaveBeenCalledWith(
      "/repo/_bmad-output/docs/other.html",
      expect.objectContaining({ openInNewTab: true }),
    );
  });

  it("ctrlKey=true sets openInNewTab", async () => {
    const onNavigate = await renderWithNav();
    dispatchIframeMessage({
      type: "codesense:navigate",
      href: "other.html",
      metaKey: false,
      ctrlKey: true,
    });
    expect(onNavigate).toHaveBeenCalledWith(
      "/repo/_bmad-output/docs/other.html",
      expect.objectContaining({ openInNewTab: true }),
    );
  });

  it("ignores messages with wrong type", async () => {
    const onNavigate = await renderWithNav();
    dispatchIframeMessage({
      type: "some-other-type",
      href: "file.md",
    });
    expect(onNavigate).not.toHaveBeenCalled();
  });

  it("codesense:close-tab closes the active file tab (⌘W forwarded from the iframe)", async () => {
    // A focused iframe swallows ⌘W, so the injected script forwards it; the
    // parent routes to the shared close primitive → closeTab(teamKey, "", path).
    await renderWithNav();
    dispatchIframeMessage({ type: "codesense:close-tab" });
    expect(mockCloseTab).toHaveBeenCalledWith("_no_team:_none", "", filePath, { user: true });
  });

  it("ignores messages from wrong source", async () => {
    const onNavigate = await renderWithNav();
    dispatchForeignMessage({
      type: "codesense:navigate",
      href: "file.md",
      metaKey: false,
      ctrlKey: false,
    });
    expect(onNavigate).not.toHaveBeenCalled();
  });

  it("codesense:external-link calls openUrl", async () => {
    await renderWithNav();
    dispatchIframeMessage({
      type: "codesense:external-link",
      href: "https://example.com",
    });
    expect(mockOpenUrl).toHaveBeenCalledWith("https://example.com");
  });

  it("external-link with protocol-relative URL prepends https:", async () => {
    await renderWithNav();
    dispatchIframeMessage({
      type: "codesense:external-link",
      href: "//example.com/page",
    });
    expect(mockOpenUrl).toHaveBeenCalledWith("https://example.com/page");
  });

  it("external-link blocks file:// scheme", async () => {
    await renderWithNav();
    dispatchIframeMessage({
      type: "codesense:external-link",
      href: "file:///etc/passwd",
    });
    expect(mockOpenUrl).not.toHaveBeenCalled();
  });

  it("external-link blocks javascript: scheme", async () => {
    await renderWithNav();
    dispatchIframeMessage({
      type: "codesense:external-link",
      href: "javascript:alert(1)",
    });
    expect(mockOpenUrl).not.toHaveBeenCalled();
  });

  it("external-link blocks malformed scheme like https:javascript:", async () => {
    await renderWithNav();
    dispatchIframeMessage({
      type: "codesense:external-link",
      href: "https:javascript:alert(1)",
    });
    expect(mockOpenUrl).not.toHaveBeenCalled();
  });

  it("external-link allows mailto:", async () => {
    await renderWithNav();
    dispatchIframeMessage({
      type: "codesense:external-link",
      href: "mailto:user@example.com",
    });
    expect(mockOpenUrl).toHaveBeenCalledWith("mailto:user@example.com");
  });

  it("ignores navigate for non-viewable extensions", async () => {
    const onNavigate = await renderWithNav();
    dispatchIframeMessage({
      type: "codesense:navigate",
      href: "data.json",
      metaKey: false,
      ctrlKey: false,
    });
    expect(onNavigate).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// SLW-FX-22: codesense:shortcut — ⌘P / ⌘⇧G reachability from the HTML iframe
// (full markdown↔html parity). The injected script forwards the chords the
// focused iframe would otherwise swallow; the parent routes each `action` to
// the same app primitive the global document listener fires.
// ---------------------------------------------------------------------------

describe("HtmlViewer codesense:shortcut router", () => {
  const filePath = "/repo/_bmad-output/docs/dashboard.html";

  function chord(key: string, shiftKey = false) {
    return {
      type: "codesense:shortcut",
      key,
      code: key === "\\" ? "Backslash" : `Key${key}`,
      metaKey: true,
      ctrlKey: false,
      shiftKey,
      altKey: false,
    };
  }
  beforeEach(() => {
    shortcutTeamKey = "acme:web";
    shortcutOverrides = {};
    shortcutWorkspace = {};
    mockDispatchMenuAction.mockClear();
  });
  afterEach(() => {
    shortcutTeamKey = "_no_team:_none";
  });
  it.each([
    ["I", false, "viewer.toggle-agent-inbox"],
    ["I", true, "viewer.toggle-inbox-rollup"],
    ["B", false, "viewer.toggle-nav"],
    ["\\", false, "viewer.toggle-terminal"],
  ])("forwards %s through the registry", async (key, shift, id) => {
    await renderWithShortcuts();
    dispatchIframeMessage(chord(key as string, shift as boolean));
    expect(mockDispatchMenuAction).toHaveBeenCalledExactlyOnceWith(id);
  });
  it("forwards Ctrl+B and Cmd+Alt+Right through the registry", async () => {
    await renderWithShortcuts();
    dispatchIframeMessage({ ...chord("B"), metaKey: false, ctrlKey: true });
    expect(mockDispatchMenuAction).toHaveBeenLastCalledWith("viewer.toggle-nav");
    dispatchIframeMessage({ ...chord("ArrowRight"), code: "ArrowRight", altKey: true });
    expect(mockDispatchMenuAction).toHaveBeenLastCalledWith("viewer.focus-pane-right");
  });
  it("honours rebindings, unmatched keys and the focused-workspace gate", async () => {
    await renderWithShortcuts();
    shortcutOverrides = { "viewer.toggle-nav": "CmdOrCtrl+Alt+B" };
    dispatchIframeMessage(chord("B"));
    dispatchIframeMessage(chord("C"));
    shortcutWorkspace = null;
    dispatchIframeMessage({ ...chord("B"), altKey: true });
    expect(mockDispatchMenuAction).toHaveBeenCalledExactlyOnceWith("viewer.toggle-nav");
  });
  it.each(["[", "]"])(
    "keeps %s navigation in the frame's editor, including rebindings",
    async (key) => {
      await renderWithShortcuts();
      dispatchIframeMessage({ ...chord(key), editorTarget: true });
      expect(mockDispatchMenuAction).not.toHaveBeenCalled();
      const id = key === "[" ? "nav.context-back" : "nav.context-forward";
      shortcutOverrides = { [id]: "CmdOrCtrl+Alt+N" };
      dispatchIframeMessage({ ...chord("N"), altKey: true, editorTarget: true });
      expect(mockDispatchMenuAction).not.toHaveBeenCalled();
      dispatchIframeMessage({ ...chord("N"), altKey: true, editorTarget: false });
      expect(mockDispatchMenuAction).toHaveBeenCalledExactlyOnceWith(id);
    },
  );
  it("keeps split shortcuts in frame editors and fields, including rebound chords", async () => {
    await renderWithShortcuts();
    dispatchIframeMessage({ ...chord("D"), textFieldTarget: true });
    dispatchIframeMessage({ ...chord("D"), editorTarget: true });
    expect(mockDispatchMenuAction).not.toHaveBeenCalled();
    shortcutOverrides = { "viewer.split-terminal": "CmdOrCtrl+Alt+N" };
    dispatchIframeMessage({ ...chord("N"), altKey: true, textFieldTarget: true });
    expect(mockDispatchMenuAction).not.toHaveBeenCalled();
    dispatchIframeMessage({ ...chord("N"), altKey: true });
    expect(mockDispatchMenuAction).toHaveBeenCalledExactlyOnceWith("viewer.split-terminal");
  });
  it("ignores foreign or malformed chord messages and forwards Inbox shortcuts", async () => {
    await renderWithShortcuts();
    dispatchForeignMessage(chord("B"));
    dispatchIframeMessage({ type: "codesense:shortcut", key: "B" });
    expect(mockDispatchMenuAction).not.toHaveBeenCalled();
    dispatchIframeMessage(chord("I", true));
    expect(mockDispatchMenuAction).toHaveBeenCalledExactlyOnceWith("viewer.toggle-inbox-rollup");
  });
  async function renderWithShortcuts(baseline = false) {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(
      <HtmlViewer
        filePath={filePath}
        teamContentPath="/repo"
        baselineSource={baseline ? { orgRoot: "/repo", relPath: "dashboard.html" } : undefined}
      />,
    );
    await waitFor(() => {
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123",
      );
    });
  }

  it("⌘P is an ordinary registry chord: file quick-open, or whatever took it", async () => {
    await renderWithShortcuts();
    dispatchIframeMessage(chord("p"));
    expect(mockDispatchMenuAction).toHaveBeenCalledExactlyOnceWith("nav.quick-open");
    shortcutOverrides = { "nav.quick-open": "", "viewer.toggle-nav": "CmdOrCtrl+P" };
    dispatchIframeMessage(chord("p"));
    expect(mockDispatchMenuAction).toHaveBeenLastCalledWith("viewer.toggle-nav");
    expect(mockDispatchMenuAction).toHaveBeenCalledTimes(2);
  });

  it("forwards Option chords, modified arrows, function keys and Escape through the registry", async () => {
    await renderWithShortcuts();
    const plain = { ...chord("B"), metaKey: false };
    shortcutOverrides = {
      "viewer.toggle-nav": "Alt+B",
      "viewer.toggle-terminal": "F6",
      "nav.switch-org": "CmdOrCtrl+ArrowDown",
    };
    dispatchIframeMessage({ ...plain, altKey: true });
    expect(mockDispatchMenuAction).toHaveBeenLastCalledWith("viewer.toggle-nav");
    dispatchIframeMessage({ ...plain, key: "F6", code: "F6" });
    expect(mockDispatchMenuAction).toHaveBeenLastCalledWith("viewer.toggle-terminal");
    dispatchIframeMessage({ ...chord("ArrowDown"), code: "ArrowDown" });
    expect(mockDispatchMenuAction).toHaveBeenLastCalledWith("nav.switch-org");
    shortcutOverrides = { "nav.switch-org": "Escape" };
    dispatchIframeMessage({ type: "codesense:key-escape" });
    expect(mockDispatchMenuAction).toHaveBeenLastCalledWith("nav.switch-org");
    expect(mockDispatchMenuAction).toHaveBeenCalledTimes(4);
  });

  it("action=switch-org toggles the workspace switcher (⌘⇧G parity, AC-2)", async () => {
    await renderWithShortcuts();
    dispatchIframeMessage(chord("G", true));
    expect(mockDispatchMenuAction).toHaveBeenCalledExactlyOnceWith("nav.switch-org");
  });

  it("routes for a baseline render too — shortcuts are UI navigation, not the bridge", async () => {
    await renderWithShortcuts(true);
    dispatchIframeMessage(chord("p"));
    dispatchIframeMessage(chord("G", true));
    expect(mockDispatchMenuAction).toHaveBeenCalledWith("nav.quick-open");
    expect(mockDispatchMenuAction).toHaveBeenCalledWith("nav.switch-org");
  });

  it("ignores a shortcut from a foreign (non-iframe) source (AC-7)", async () => {
    await renderWithShortcuts();
    dispatchForeignMessage(chord("p"));
    dispatchForeignMessage(chord("G", true));
    expect(mockDispatchMenuAction).not.toHaveBeenCalled();
    expect(mockToggleOrgSwitcher).not.toHaveBeenCalled();
  });

  it("a message without a key description is a safe no-op (AC-3)", async () => {
    await renderWithShortcuts();
    expect(() =>
      dispatchIframeMessage({ type: "codesense:shortcut", action: "not-a-real-action" }),
    ).not.toThrow();
    expect(mockDispatchMenuAction).not.toHaveBeenCalled();
    expect(mockToggleOrgSwitcher).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// Scroll capture/restore (SW-FX-5)
// ---------------------------------------------------------------------------

/**
 * happy-dom does not populate `contentWindow` on iframes, so we stub it with
 * a `postMessage` spy. Returns the spy + the fake window to use as the
 * message `source` so the parent's `event.source` guard passes.
 */
function stubContentWindow() {
  const iframe = document.querySelector("iframe")!;
  const postMessage = vi.fn();
  const fakeWindow = { postMessage } as unknown as Window;
  Object.defineProperty(iframe, "contentWindow", {
    value: fakeWindow,
    writable: true,
    configurable: true,
  });
  return { iframe, fakeWindow, postMessage };
}

describe("HtmlViewer scroll capture/restore", () => {
  const filePath = "/repo/_bmad-output/docs/long.html";

  async function renderViewer() {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(<HtmlViewer filePath={filePath} teamContentPath="/repo" />);
    await waitFor(() => {
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123",
      );
    });
  }

  it("tracks a codesense:scroll report and restores it on the next load", async () => {
    await renderViewer();
    const { iframe, fakeWindow, postMessage } = stubContentWindow();

    // Injected script reports the reader's scroll position.
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:scroll", y: 250 },
        source: fakeWindow,
      }),
    );

    // A refresh-key bump reloads the iframe → onLoad fires.
    fireEvent.load(iframe);
    // Restores wait for the new document's transport handshake and keep its pinned origin.
    window.dispatchEvent(
      new FrameMessageEvent("message", { data: { type: "codesense:ready" }, source: fakeWindow }),
    );

    expect(postMessage).toHaveBeenCalledWith(
      { type: "codesense:restore-scroll", y: 250 },
      "http://f-test.localhost:9123",
    );
  });

  it("ignores a codesense:scroll message from the wrong source", async () => {
    await renderViewer();
    const { iframe, fakeWindow, postMessage } = stubContentWindow();

    // Wrong source — must not update tracked scroll state.
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:scroll", y: 999 },
        source: {} as Window,
      }),
    );

    fireEvent.load(iframe);
    // Restores wait for the new document's transport handshake and keep its pinned origin.
    window.dispatchEvent(
      new FrameMessageEvent("message", { data: { type: "codesense:ready" }, source: fakeWindow }),
    );

    // Restores the default (0), not the spoofed 999.
    expect(postMessage).toHaveBeenCalledWith(
      { type: "codesense:restore-scroll", y: 0 },
      "http://f-test.localhost:9123",
    );
  });

  it("ignores a non-finite scroll offset (NaN passes typeof number)", async () => {
    await renderViewer();
    const { iframe, fakeWindow, postMessage } = stubContentWindow();

    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:scroll", y: Number.NaN },
        source: fakeWindow,
      }),
    );

    fireEvent.load(iframe);
    // Restores wait for the new document's transport handshake and keep its pinned origin.
    window.dispatchEvent(
      new FrameMessageEvent("message", { data: { type: "codesense:ready" }, source: fakeWindow }),
    );

    // NaN rejected by the finite guard → restore stays at the default 0.
    expect(postMessage).toHaveBeenCalledWith(
      { type: "codesense:restore-scroll", y: 0 },
      "http://f-test.localhost:9123",
    );
  });
});

// ---------------------------------------------------------------------------
// SW-LD-1: codesense bridge (postMessage RPC)
// ---------------------------------------------------------------------------

describe("HtmlViewer load pre-check", () => {
  const filePath = "/repo/panes/board.html";

  function deferredFetch() {
    let reject!: (err: unknown) => void;
    mockFetch.mockReturnValue(
      new Promise((_, rej) => {
        reject = rej;
      }),
    );
    return (err: unknown) => reject(err);
  }

  async function renderPane(onDocProblem = vi.fn(), onDocLoaded = vi.fn(), onDocLive = vi.fn()) {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(
      <HtmlViewer
        filePath={filePath}
        teamContentPath="/repo/content"
        pane={{ contentRoot: "/repo/content", panesDir: "/repo/panes", fileName: "board.html" }}
        onDocProblem={onDocProblem}
        onDocLoaded={onDocLoaded}
        onDocLive={onDocLive}
      />,
    );
    await waitFor(() => expect(mockFetch).toHaveBeenCalled());
    const iframe = document.querySelector("iframe");
    const fakeWindow = { postMessage: vi.fn() } as unknown as Window;
    if (iframe) {
      Object.defineProperty(iframe, "contentWindow", { value: fakeWindow, configurable: true });
    }
    const announce = () =>
      window.dispatchEvent(
        new FrameMessageEvent("message", { data: { type: "codesense:ready" }, source: fakeWindow }),
      );
    return { onDocProblem, onDocLoaded, onDocLive, announce, fakeWindow };
  }

  it("counts explicit page ready as live, independently of the automatic transport handshake", async () => {
    const { announce, fakeWindow, onDocLive, onDocLoaded } = await renderPane();
    announce();
    expect(onDocLoaded).toHaveBeenCalledOnce();
    expect(onDocLive).not.toHaveBeenCalled();
    window.dispatchEvent(
      new FrameMessageEvent("message", { data: { type: "codesense:live" }, source: fakeWindow }),
    );
    expect(onDocLive).toHaveBeenCalledOnce();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("a network-level pre-check failure is dropped when the doc then announces itself", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const fail = deferredFetch();
    const { onDocProblem, onDocLoaded, announce } = await renderPane();
    fail(new TypeError("Load failed"));
    await Promise.resolve();
    announce();
    expect(onDocLoaded).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(NETWORK_PROBLEM_GRACE_MS + 1);
    expect(onDocProblem).not.toHaveBeenCalled();
  });

  it("a speculative pane pre-check failure leaves the startup deadline to its slot", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const fail = deferredFetch();
    const { onDocProblem } = await renderPane();
    fail(new TypeError("Load failed"));
    await waitFor(() => expect(vi.getTimerCount()).toBeGreaterThan(0));
    expect(onDocProblem).not.toHaveBeenCalled();
    vi.advanceTimersByTime(NETWORK_PROBLEM_GRACE_MS);
    expect(onDocProblem).not.toHaveBeenCalled();
  });

  it("a real HTTP failure still reports at once", async () => {
    mockFetch.mockResolvedValue({ ok: false, status: 500, statusText: "Internal Server Error" });
    const { onDocProblem } = await renderPane();
    await waitFor(() =>
      expect(onDocProblem).toHaveBeenCalledWith({
        kind: "load",
        message: `${filePath}: HTTP 500 Internal Server Error`,
      }),
    );
  });

  it("a 404 shows the missing placeholder, not a load problem", async () => {
    mockFetch.mockResolvedValue({ ok: false, status: 404, statusText: "Not Found" });
    const { onDocProblem } = await renderPane();
    await waitFor(() =>
      expect(screen.getByText("This file was removed from disk")).toBeInTheDocument(),
    );
    expect(onDocProblem).not.toHaveBeenCalled();
  });
});

describe("HtmlViewer codesense bridge (SW-LD-1)", () => {
  const filePath = "/repo/dashboard.html";

  async function renderAndStub(baseline = false) {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(
      <HtmlViewer
        filePath={filePath}
        teamContentPath="/repo"
        baselineSource={baseline ? { orgRoot: "/repo", relPath: "dashboard.html" } : undefined}
      />,
    );
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123/",
      ),
    );
    const iframe = document.querySelector("iframe")!;
    const postMessage = vi.fn();
    const fakeWindow = { postMessage } as unknown as Window;
    Object.defineProperty(iframe, "contentWindow", {
      value: fakeWindow,
      writable: true,
      configurable: true,
    });
    return { fakeWindow, postMessage };
  }

  it.each(["dark", "light"] as const)(
    "passes its resolved %s theme to the hosted message document URL",
    async (scheme) => {
      mockGetHttpApiPort.mockResolvedValue(9123);
      const theme = { scheme, tokens: { "--cs-bg": scheme === "dark" ? "#101820" : "#fafafa" } };
      render(
        <HtmlViewer
          filePath={filePath}
          teamContentPath="/repo"
          message={{ org: "alderco", team: "riverside", messageId: 42, sender: "agent" }}
          hostTheme={theme}
        />,
      );
      await waitFor(() => {
        const src = document.querySelector("iframe")?.getAttribute("src");
        expect(src).toContain("/message/");
        expect(src).toContain(`theme=${encodeURIComponent(JSON.stringify(theme))}`);
      });
    },
  );

  it("a centre doc that announces itself hears its visibility and the theme, without the pane's root scheme", async () => {
    const { fakeWindow, postMessage } = await renderAndStub();
    window.dispatchEvent(
      new FrameMessageEvent("message", { data: { type: "codesense:ready" }, source: fakeWindow }),
    );
    await waitFor(() =>
      expect(postMessage).toHaveBeenCalledWith(
        { type: "codesense:visibility", visible: true },
        "http://f-test.localhost:9123",
      ),
    );
    // A centre dashboard gets the app's tokens too; its root takes the app's
    // `color-scheme` only if the doc itself uses them (the shim decides), and
    // the frame keeps its own scheme.
    expect(postMessage).toHaveBeenCalledWith(
      {
        type: "codesense:theme",
        theme: expect.objectContaining({ scheme: expect.stringMatching(/^(dark|light)$/) }),
        applyScheme: false,
      },
      "http://f-test.localhost:9123",
    );
    expect(postMessage).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "codesense:theme", theme: null }),
      "http://f-test.localhost:9123",
    );
    expect(document.querySelector("iframe")!.style.colorScheme).toBe("");
  });

  it("a pane that announces itself hears the theme with its root scheme, and its frame matches", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(
      <HtmlViewer
        filePath="/repo/panes/board.html"
        teamContentPath="/repo/content"
        pane={{ contentRoot: "/repo/content", panesDir: "/repo/panes", fileName: "board.html" }}
      />,
    );
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123/",
      ),
    );
    const iframe = document.querySelector("iframe")!;
    const postMessage = vi.fn();
    const fakeWindow = { postMessage } as unknown as Window;
    Object.defineProperty(iframe, "contentWindow", { value: fakeWindow, configurable: true });
    window.dispatchEvent(
      new FrameMessageEvent("message", { data: { type: "codesense:ready" }, source: fakeWindow }),
    );
    await waitFor(() =>
      expect(postMessage).toHaveBeenCalledWith(
        expect.objectContaining({ type: "codesense:theme", applyScheme: true }),
        "http://f-test.localhost:9123",
      ),
    );
    expect(iframe.style.colorScheme).toMatch(/^(dark|light)$/);
  });

  it("an allowlisted invoke returns the command result (AC-2)", async () => {
    const { fakeWindow, postMessage } = await renderAndStub();

    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:invoke", id: 7, command: "getDependencyStatus", args: null },
        source: fakeWindow,
      }),
    );

    await waitFor(() => {
      expect(postMessage).toHaveBeenCalledWith(
        { type: "codesense:invoke-result", id: 7, ok: true, data: { ok: true } },
        // Reply is pinned to the doc's real origin, never "*".
        "http://f-test.localhost:9123",
      );
    });
    expect(mockGetDependencyStatus).toHaveBeenCalledTimes(1);
  });

  it("a NON-allowlisted invoke is rejected, never reaching a command (AC-2)", async () => {
    const { fakeWindow, postMessage } = await renderAndStub();

    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:invoke", id: 9, command: "deleteEverything", args: null },
        source: fakeWindow,
      }),
    );

    await waitFor(() => {
      expect(postMessage).toHaveBeenCalledWith(
        expect.objectContaining({ type: "codesense:invoke-result", id: 9, ok: false }),
        "http://f-test.localhost:9123",
      );
    });
    const reply = postMessage.mock.calls[0][0];
    expect(reply.error).toContain("not allowlisted");
    expect(mockGetDependencyStatus).not.toHaveBeenCalled();
  });

  it("baseline render: invoke is denied — a historical view has no bridge", async () => {
    const { fakeWindow, postMessage } = await renderAndStub(true);

    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:invoke", id: 3, command: "getDependencyStatus", args: null },
        source: fakeWindow,
      }),
    );

    await waitFor(() => {
      expect(postMessage).toHaveBeenCalledWith(
        {
          type: "codesense:invoke-result",
          id: 3,
          ok: false,
          error: "unavailable: baseline view has no bridge",
        },
        // A baseline frame renders opaque-origin ("null"), which can never match
        // a pinned origin — the contentless rejection uses "*" so it actually
        // reaches the frame instead of hanging its invoke() promise.
        "*",
      );
    });
    expect(mockGetDependencyStatus).not.toHaveBeenCalled();
  });

  it("ignores an invoke with a non-numeric (forged) id", async () => {
    const { fakeWindow, postMessage } = await renderAndStub();
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:invoke", id: {}, command: "getDependencyStatus" },
        source: fakeWindow,
      }),
    );
    await new Promise((r) => setTimeout(r, 0));
    expect(mockGetDependencyStatus).not.toHaveBeenCalled();
    expect(postMessage).not.toHaveBeenCalled();
  });

  it("ignores an invoke from the wrong source", async () => {
    await renderAndStub();
    const stray = vi.fn();
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:invoke", id: 1, command: "getDependencyStatus" },
        source: { postMessage: stray } as unknown as Window,
      }),
    );
    // Give any (incorrect) async handling a tick.
    await new Promise((r) => setTimeout(r, 0));
    expect(stray).not.toHaveBeenCalled();
    expect(mockGetDependencyStatus).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// Action bridge: every allowlisted verb runs when called — no manifest, no
// grant, no confirm. The audit line + runaway rate guards live in use-action-gate.
// ---------------------------------------------------------------------------

describe("HtmlViewer action bridge", () => {
  const filePath = "/repo/dashboard.html";

  async function renderAndStub() {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(<HtmlViewer filePath={filePath} teamContentPath="/repo" />);
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123/",
      ),
    );
    const iframe = document.querySelector("iframe")!;
    const postMessage = vi.fn();
    const fakeWindow = { postMessage } as unknown as Window;
    Object.defineProperty(iframe, "contentWindow", {
      value: fakeWindow,
      writable: true,
      configurable: true,
    });
    return { fakeWindow, postMessage };
  }

  function invoke(fakeWindow: Window, id: number, command: string, args: unknown) {
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:invoke", id, command, args },
        source: fakeWindow,
      }),
    );
  }

  const lastReply = (pm: ReturnType<typeof vi.fn>) => pm.mock.calls.at(-1)![0];

  it("a setStatus with no manifest flips status via applyMetadataEdit(setField)", async () => {
    const { fakeWindow, postMessage } = await renderAndStub();
    invoke(fakeWindow, 4, "setStatus", { path: "/repo/story.md", status: "done" });
    await waitFor(() => expect(lastReply(postMessage).ok).toBe(true));
    expect(mockApplyMetadataEdit).toHaveBeenCalledWith("/repo/story.md", [
      { kind: "setField", field: "status", value: "done" },
    ]);
    // The doc is never read for a capability manifest.
    expect(mockReadFile).not.toHaveBeenCalled();
  });

  it("a cs:capabilities meta is ignored — an undeclared verb still runs", async () => {
    mockReadFile.mockResolvedValue(
      '<html><head><meta name="cs:capabilities" content="draft"></head></html>',
    );
    const { fakeWindow, postMessage } = await renderAndStub();
    invoke(fakeWindow, 5, "setStatus", { path: "/repo/s.md", status: "done" });
    await waitFor(() => expect(lastReply(postMessage).ok).toBe(true));
    expect(mockApplyMetadataEdit).toHaveBeenCalledTimes(1);
  });

  it("a batch setStatus applies every item and returns a per-item result array", async () => {
    const { fakeWindow, postMessage } = await renderAndStub();
    invoke(fakeWindow, 6, "setStatus", { path: ["/a.md", "/b.md", "/c.md"], status: "done" });
    await waitFor(() => expect(lastReply(postMessage).ok).toBe(true));
    expect(mockApplyMetadataEdit).toHaveBeenCalledTimes(3);
    expect(lastReply(postMessage).data).toEqual([
      { path: "/a.md", ok: true },
      { path: "/b.md", ok: true },
      { path: "/c.md", ok: true },
    ]);
  });

  it("surfaces a Rust reject verbatim", async () => {
    mockApplyMetadataEdit.mockRejectedValueOnce(
      new Error("refusing to edit metadata: /x is a read-only projected/installed view"),
    );
    const { fakeWindow, postMessage } = await renderAndStub();
    invoke(fakeWindow, 7, "setStatus", { path: "/x", status: "done" });
    await waitFor(() => {
      const r = lastReply(postMessage);
      expect(r.ok).toBe(false);
      expect(r.error).toContain("refusing to edit metadata");
    });
  });

  it("openWorkspace forwards straight to the launch controller (unavailable with none mounted)", async () => {
    const { fakeWindow, postMessage } = await renderAndStub();
    invoke(fakeWindow, 8, "openWorkspace", { storyId: "SW-DX-7" });
    // No dashboard-launch controller is registered in this render → the forward
    // rejects `unavailable` (never a silent resolve). The real launch is covered
    // by the no-grants seam test and dashboard-launch.test.ts.
    await waitFor(() => {
      const r = lastReply(postMessage);
      expect(r.ok).toBe(false);
      expect(r.error).toBe("unavailable: no active dashboard workspace context");
    });
  });

  it("a FORGED action-shaped command is refused by the allowlist", async () => {
    const { fakeWindow, postMessage } = await renderAndStub();
    invoke(fakeWindow, 10, "rm_rf", { path: "/etc" });
    await waitFor(() => {
      const r = lastReply(postMessage);
      expect(r.ok).toBe(false);
      expect(r.error).toContain("not allowlisted");
    });
    expect(mockApplyMetadataEdit).not.toHaveBeenCalled();
  });

  it("an allowed action and a refused one each settle in the native audit", async () => {
    const { fakeWindow, postMessage } = await renderAndStub();
    invoke(fakeWindow, 11, "setStatus", { path: "/ok.md", status: "done" });
    await waitFor(() => expect(lastReply(postMessage).ok).toBe(true));
    expect(nativeSettle).toHaveBeenCalledWith("call", true, "admitted");
    mockApplyMetadataEdit.mockRejectedValueOnce(new Error("refusing to edit metadata: /x"));
    invoke(fakeWindow, 12, "setStatus", { path: "/x", status: "done" });
    await waitFor(() => expect(lastReply(postMessage).ok).toBe(false));
    expect(nativeSettle).toHaveBeenCalledWith("call", false, "refusing to edit metadata: /x");
  });

  it("reads are audit-exempt", async () => {
    const { fakeWindow, postMessage } = await renderAndStub();
    invoke(fakeWindow, 13, "getDependencyStatus", null);
    await waitFor(() => expect(lastReply(postMessage).ok).toBe(true));
    expect(mockLoggerInfo).not.toHaveBeenCalledWith("dashboard-action", expect.anything());
    expect(mockLoggerWarn).not.toHaveBeenCalledWith("dashboard-action", expect.anything());
  });
});

// ---------------------------------------------------------------------------
// changedPaths reload gate (SW-FX-5 AC-7)
// ---------------------------------------------------------------------------

describe("HtmlViewer changedPaths reload gate", () => {
  const filePath = "/repo/docs/page.html";

  it("does not change iframe src when a refreshKey bump excludes this file", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    const other = new Set(["/repo/docs/other.html"]);
    const { rerender } = render(
      <HtmlViewer
        filePath={filePath}
        refreshKey={1}
        teamContentPath="/repo"
        changedPaths={other}
      />,
    );
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123/",
      ),
    );
    const src1 = document.querySelector("iframe")!.getAttribute("src");
    expect(src1).toContain("v=1");

    // Watcher tick for an unrelated file: refreshKey bumps but our file is
    // not in changedPaths → src (and the embedded v=) must stay put.
    rerender(
      <HtmlViewer
        filePath={filePath}
        refreshKey={2}
        teamContentPath="/repo"
        changedPaths={new Set(["/repo/docs/other.html"])}
      />,
    );
    await waitFor(() => {
      expect(document.querySelector("iframe")!.getAttribute("src")).toBe(src1);
    });
  });

  it("changes iframe src when the refreshKey bump includes this file", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    const { rerender } = render(
      <HtmlViewer
        filePath={filePath}
        refreshKey={1}
        teamContentPath="/repo"
        changedPaths={new Set(["/repo/docs/other.html"])}
      />,
    );
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123/",
      ),
    );
    const src1 = document.querySelector("iframe")!.getAttribute("src");

    rerender(
      <HtmlViewer
        filePath={filePath}
        refreshKey={2}
        teamContentPath="/repo"
        changedPaths={new Set([filePath])}
      />,
    );
    await waitFor(() => {
      const src2 = document.querySelector("iframe")!.getAttribute("src");
      expect(src2).not.toBe(src1);
      expect(src2).toContain("v=2");
    });
  });

  it("falls back to reloading when changedPaths is absent", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    const { rerender } = render(
      <HtmlViewer filePath={filePath} refreshKey={1} teamContentPath="/repo" />,
    );
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123/",
      ),
    );
    const src1 = document.querySelector("iframe")!.getAttribute("src");

    rerender(<HtmlViewer filePath={filePath} refreshKey={2} teamContentPath="/repo" />);
    await waitFor(() => {
      const src2 = document.querySelector("iframe")!.getAttribute("src");
      expect(src2).not.toBe(src1);
      expect(src2).toContain("v=2");
    });
  });
});

// ---------------------------------------------------------------------------
// SW-SD-1: snapshot to context (chrome button + doc-initiated verb)
// ---------------------------------------------------------------------------

// Absolute archive path: `${teamContentPath}/dashboards/snapshots/{stem}/{ts}-{nonce}.html`.
// The `-{nonce}` suffix (review r1) makes same-millisecond captures distinct.
const ARCHIVE_RE = /^\/repo\/dashboards\/snapshots\/dashboard\/\d{8}T\d{9}Z-[a-z0-9]{1,7}\.html$/;

describe("HtmlViewer snapshot button (SW-SD-1)", () => {
  const filePath = "/repo/docs/dashboard.html";

  async function renderViewer() {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(<HtmlViewer filePath={filePath} teamContentPath="/repo" />);
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123/",
      ),
    );
  }

  it("shows the Snapshot button for a live doc (AC-1/AC-3)", async () => {
    await renderViewer();
    expect(screen.getByRole("button", { name: /snapshot/i })).toBeInTheDocument();
  });

  it("SW-FX-62: docks the Snapshot button in normal flow, not an absolute overlay", async () => {
    await renderViewer();
    const btn = screen.getByRole("button", { name: /snapshot/i });
    // The button must not float over the iframe content anymore.
    expect(btn.className).not.toMatch(/\babsolute\b/);
    // It lives in a normal-flow band that is a sibling of the iframe (not a
    // descendant that could paint over it).
    const iframe = document.querySelector("iframe")!;
    expect(btn.compareDocumentPosition(iframe) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("hides the Snapshot button for a baseline/diff render (AC-3)", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(
      <HtmlViewer
        filePath={filePath}
        teamContentPath="/repo"
        baselineSource={{ orgRoot: "/repo", relPath: "docs/dashboard.html" }}
      />,
    );
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123/",
      ),
    );
    expect(screen.queryByRole("button", { name: /snapshot/i })).not.toBeInTheDocument();
  });

  it("click posts an origin-pinned snapshot-request, then archives the returned HTML to an ABSOLUTE path (AC-1/AC-4)", async () => {
    await renderViewer();
    const iframe = document.querySelector("iframe")!;
    const postMessage = vi.fn();
    const fakeWindow = { postMessage } as unknown as Window;
    Object.defineProperty(iframe, "contentWindow", {
      value: fakeWindow,
      writable: true,
      configurable: true,
    });

    fireEvent.click(screen.getByRole("button", { name: /snapshot/i }));

    // The request is posted into the doc, pinned to its real origin (never "*").
    expect(postMessage).toHaveBeenCalledTimes(1);
    const [msg, origin] = postMessage.mock.calls[0];
    expect(msg.type).toBe("codesense:snapshot-request");
    expect(typeof msg.id).toBe("number");
    expect(origin).toBe("http://f-test.localhost:9123");

    // The doc answers with the frozen HTML → parent writes it to context.
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: {
          type: "codesense:snapshot-result",
          id: msg.id,
          ok: true,
          html: "<!doctype html><p>frozen",
        },
        source: fakeWindow,
        origin: "http://f-test.localhost:9123",
      }),
    );

    await waitFor(() => expect(mockWriteContentFile).toHaveBeenCalledTimes(1));
    const [path, content] = mockWriteContentFile.mock.calls[0];
    expect(path).toMatch(ARCHIVE_RE); // absolute, rooted at teamContentPath
    expect(content).toBe("<!doctype html><p>frozen");
    expect(mockToastSuccess).toHaveBeenCalled();
  });

  it("surfaces an error toast when the doc reports snapshot failure (no write)", async () => {
    await renderViewer();
    const iframe = document.querySelector("iframe")!;
    const postMessage = vi.fn();
    const fakeWindow = { postMessage } as unknown as Window;
    Object.defineProperty(iframe, "contentWindow", {
      value: fakeWindow,
      writable: true,
      configurable: true,
    });

    fireEvent.click(screen.getByRole("button", { name: /snapshot/i }));
    const { id } = postMessage.mock.calls[0][0];
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:snapshot-result", id, ok: false, error: "serialize blew up" },
        source: fakeWindow,
        origin: "http://f-test.localhost:9123",
      }),
    );

    await waitFor(() => expect(mockToastError).toHaveBeenCalled());
    expect(mockWriteContentFile).not.toHaveBeenCalled();
  });
});

describe("HtmlViewer doc-initiated snapshot() verb (SW-SD-1)", () => {
  const filePath = "/repo/docs/dashboard.html";

  async function renderAndStub(baseline = false) {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(
      <HtmlViewer
        filePath={filePath}
        teamContentPath="/repo"
        baselineSource={baseline ? { orgRoot: "/repo", relPath: "docs/dashboard.html" } : undefined}
      />,
    );
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123/",
      ),
    );
    const iframe = document.querySelector("iframe")!;
    const postMessage = vi.fn();
    const fakeWindow = { postMessage } as unknown as Window;
    Object.defineProperty(iframe, "contentWindow", {
      value: fakeWindow,
      writable: true,
      configurable: true,
    });
    return { fakeWindow, postMessage };
  }

  it("archives the doc's HTML and acks origin-pinned so the Promise resolves (AC-2)", async () => {
    const { fakeWindow, postMessage } = await renderAndStub();

    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:snapshot", id: 11, html: "<!doctype html><main>state" },
        source: fakeWindow,
      }),
    );

    await waitFor(() => expect(mockWriteContentFile).toHaveBeenCalledTimes(1));
    const [path, content] = mockWriteContentFile.mock.calls[0];
    expect(path).toMatch(ARCHIVE_RE);
    expect(content).toBe("<!doctype html><main>state");
    await waitFor(() =>
      expect(postMessage).toHaveBeenCalledWith(
        { type: "codesense:snapshot-result", id: 11, ok: true },
        "http://f-test.localhost:9123",
      ),
    );
  });

  it("two captures of the same doc archive to DISTINCT paths (review r1: no same-ms overwrite)", async () => {
    const { fakeWindow } = await renderAndStub();
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:snapshot", id: 21, html: "<!doctype html>a" },
        source: fakeWindow,
      }),
    );
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:snapshot", id: 22, html: "<!doctype html>b" },
        source: fakeWindow,
      }),
    );
    await waitFor(() => expect(mockWriteContentFile).toHaveBeenCalledTimes(2));
    const p1 = mockWriteContentFile.mock.calls[0][0];
    const p2 = mockWriteContentFile.mock.calls[1][0];
    expect(p1).toMatch(ARCHIVE_RE);
    expect(p2).toMatch(ARCHIVE_RE);
    expect(p1).not.toBe(p2); // the nonce suffix disambiguates even within one ms
  });

  it("baseline render: denies the write and replies at origin '*' so the Promise rejects (never hangs)", async () => {
    const { fakeWindow, postMessage } = await renderAndStub(true);

    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:snapshot", id: 12, html: "<!doctype html>x" },
        source: fakeWindow,
      }),
    );

    await waitFor(() =>
      expect(postMessage).toHaveBeenCalledWith(
        {
          type: "codesense:snapshot-result",
          id: 12,
          ok: false,
          error: "unavailable: baseline view has no bridge",
        },
        "*",
      ),
    );
    expect(mockWriteContentFile).not.toHaveBeenCalled();
  });

  it("ignores a doc-initiated snapshot with a non-numeric (forged) id", async () => {
    const { fakeWindow, postMessage } = await renderAndStub();
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:snapshot", id: {}, html: "<!doctype html>x" },
        source: fakeWindow,
      }),
    );
    await new Promise((r) => setTimeout(r, 0));
    expect(mockWriteContentFile).not.toHaveBeenCalled();
    expect(postMessage).not.toHaveBeenCalled();
  });

  it("ignores a snapshot message from the wrong source", async () => {
    await renderAndStub();
    const stray = vi.fn();
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: { type: "codesense:snapshot", id: 5, html: "<!doctype html>x" },
        source: { postMessage: stray } as unknown as Window,
      }),
    );
    await new Promise((r) => setTimeout(r, 0));
    expect(stray).not.toHaveBeenCalled();
    expect(mockWriteContentFile).not.toHaveBeenCalled();
  });
});

// RF-perf-15 (F-1): the iframe hosts a full nested WebCore document + its own
// compositor/GPU layers + decoded-image cache. React just detaches the element;
// WebKit retains the child document past detach. Blanking `src` on teardown
// forces that document (and its native backing) to tear down deterministically.
describe("RF-perf-15 (F-1): iframe native-backing reclamation on teardown", () => {
  it("blanks the iframe src to about:blank on unmount", async () => {
    const { unmount } = render(<HtmlViewer filePath="/repo/page.html" teamContentPath="/repo" />);
    let iframe!: HTMLIFrameElement;
    await waitFor(() => {
      iframe = document.querySelector("iframe")!;
      expect(iframe).toBeTruthy();
      expect(iframe.getAttribute("src")).toBeTruthy();
    });

    // Spy the instance `src` setter — the real (happy-dom) setter simulates an
    // iframe navigation, which is irrelevant here and noisy; we only assert the
    // teardown blanks it. The spy shadows the prototype accessor for this node.
    const setSrc = vi.fn();
    Object.defineProperty(iframe, "src", {
      configurable: true,
      get: () => "about:blank",
      set: setSrc,
    });

    unmount();

    // The ref cleanup blanks the captured element once it has left the document
    // (the same cleanup path fires on the baseline-toggle remount).
    await Promise.resolve();
    expect(setSrc).toHaveBeenCalledWith("about:blank");
  });

  it("keeps the src of a node React re-attaches without unmounting (StrictMode)", async () => {
    render(
      <StrictMode>
        <HtmlViewer filePath="/repo/page.html" teamContentPath="/repo" />
      </StrictMode>,
    );
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain("/viewer/"),
    );
    await new Promise((r) => setTimeout(r, 0));
    expect(document.querySelector("iframe")?.getAttribute("src")).toContain("/viewer/");
  });
});

describe("rendered document MCP capture", () => {
  const path = "/repo/docs/dashboard.html";
  async function setup(baseline = false, pane = false, captureUnavailable?: string) {
    mockGetHttpApiPort.mockResolvedValue(9123);
    const view = render(
      <HtmlViewer
        filePath={path}
        teamContentPath="/repo"
        captureUnavailable={captureUnavailable}
        pane={
          pane
            ? { contentRoot: "/repo", panesDir: "/repo/docs", fileName: "dashboard.html" }
            : undefined
        }
        baselineSource={baseline ? { orgRoot: "/repo", relPath: "docs/dashboard.html" } : undefined}
      />,
    );
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123/",
      ),
    );
    const iframe = document.querySelector("iframe")!;
    const postMessage = vi.fn();
    const source = { postMessage } as unknown as Window;
    Object.defineProperty(iframe, "contentWindow", { value: source, configurable: true });
    vi.spyOn(iframe, "getClientRects").mockReturnValue([{}] as unknown as DOMRectList);
    return { ...view, iframe, postMessage, source };
  }
  it.each(["text", "html"] as const)(
    "reads %s without archive writes and ignores forged replies",
    async (format) => {
      const { iframe, postMessage, source } = await setup();
      const resolve = vi.fn().mockResolvedValue(undefined);
      const pending = runRenderedDocumentRequest({ requestId: "read", path, format }, resolve);
      const { id } = postMessage.mock.calls[0][0];
      const data = {
        type: "codesense:snapshot-result",
        id,
        ok: true,
        content: "Live data",
        sourceUrl: iframe.src + "#anchor",
        capturedAt: "2026-10-02T12:00:00Z",
        truncated: false,
      };
      window.dispatchEvent(
        new FrameMessageEvent("message", { data, source, origin: "https://wrong.example" }),
      );
      window.dispatchEvent(
        new FrameMessageEvent("message", {
          data,
          source: window,
          origin: "http://f-test.localhost:9123",
        }),
      );
      window.dispatchEvent(
        new FrameMessageEvent("message", {
          data: { ...data, id: id + 1 },
          source,
          origin: "http://f-test.localhost:9123",
        }),
      );
      await Promise.resolve();
      expect(resolve).not.toHaveBeenCalled();
      window.dispatchEvent(
        new FrameMessageEvent("message", { data, source, origin: "http://f-test.localhost:9123" }),
      );
      await pending;
      expect(resolve).toHaveBeenCalledWith(
        "read",
        { content: "Live data", capturedAt: data.capturedAt, truncated: false },
        null,
      );
      expect(mockWriteContentFile).not.toHaveBeenCalled();
    },
  );
  it("captures an already-open pane through its pane route", async () => {
    const { iframe, postMessage, source } = await setup(false, true);
    expect(iframe.src).toContain("/pane/");
    const resolve = vi.fn().mockResolvedValue(undefined);
    const pending = runRenderedDocumentRequest(
      { requestId: "pane", path, format: "text" },
      resolve,
    );
    const { id } = postMessage.mock.calls[0][0];
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        data: {
          type: "codesense:snapshot-result",
          id,
          ok: true,
          content: "All sessions · 2 sessions",
          sourceUrl: iframe.src,
          capturedAt: "now",
          truncated: false,
        },
        source,
        origin: "http://f-test.localhost:9123",
      }),
    );
    await pending;
    expect(resolve).toHaveBeenCalledWith(
      "pane",
      expect.objectContaining({ content: "All sessions · 2 sessions" }),
      null,
    );
    expect(mockWriteContentFile).not.toHaveBeenCalled();
  });
  it("refuses capture while a retry frame still shows fallback", async () => {
    const { postMessage } = await setup(false, true, "Pane is showing fallback: retrying");
    const resolve = vi.fn().mockResolvedValue(undefined);
    await runRenderedDocumentRequest({ requestId: "retry", path, format: "text" }, resolve);
    expect(resolve).toHaveBeenCalledWith("retry", null, "Pane is showing fallback: retrying");
    expect(postMessage).not.toHaveBeenCalled();
  });
  it("excludes historical (baseline) viewers", async () => {
    const { postMessage } = await setup(true);
    const resolve = vi.fn().mockResolvedValue(undefined);
    await runRenderedDocumentRequest({ requestId: "read", path, format: "text" }, resolve);
    expect(resolve).toHaveBeenCalledWith(
      "read",
      null,
      expect.stringMatching(/Open the current HTML document/),
    );
    expect(postMessage).not.toHaveBeenCalled();
  });
  it("rejects hidden keepalive frames", async () => {
    const { iframe } = await setup();
    vi.mocked(iframe.getClientRects).mockReturnValue([] as unknown as DOMRectList);
    const resolve = vi.fn().mockResolvedValue(undefined);
    await runRenderedDocumentRequest({ requestId: "read", path, format: "text" }, resolve);
    expect(resolve).toHaveBeenCalledWith(
      "read",
      null,
      expect.stringMatching(/Select this document/),
    );
  });
  it.each(["hidden", "error"])(
    "rejects a response when %s without losing the original error",
    async (reason) => {
      const { iframe, postMessage, source } = await setup();
      const resolve = vi.fn().mockResolvedValue(undefined);
      const pending = runRenderedDocumentRequest(
        { requestId: "read", path, format: "text" },
        resolve,
      );
      const { id } = postMessage.mock.calls[0][0];
      if (reason === "hidden")
        vi.mocked(iframe.getClientRects).mockReturnValue([] as unknown as DOMRectList);
      const data =
        reason === "error"
          ? { type: "codesense:snapshot-result", id, ok: false, error: "Document is still loading" }
          : {
              type: "codesense:snapshot-result",
              id,
              ok: true,
              content: "Live data",
              sourceUrl: iframe.src,
            };
      window.dispatchEvent(
        new FrameMessageEvent("message", { data, source, origin: "http://f-test.localhost:9123" }),
      );
      await pending;
      expect(resolve).toHaveBeenCalledWith(
        "read",
        null,
        expect.stringMatching(reason === "error" ? /still loading/ : /hidden/),
      );
      expect(mockWriteContentFile).not.toHaveBeenCalled();
    },
  );
  it.each(["unmount", "navigation", "timeout"])("settles captures on %s", async (reason) => {
    const { unmount, iframe } = await setup();
    vi.useFakeTimers();
    try {
      const resolve = vi.fn().mockResolvedValue(undefined);
      const pending = runRenderedDocumentRequest(
        { requestId: "read", path, format: "text" },
        resolve,
      );
      if (reason === "unmount") unmount();
      else if (reason === "navigation") fireEvent.load(iframe);
      else await vi.advanceTimersByTimeAsync(10001);
      await pending;
      expect(resolve).toHaveBeenCalledWith(
        "read",
        null,
        expect.stringMatching(/closed|navigated|timed out/i),
      );
    } finally {
      vi.useRealTimers();
    }
    expect(mockWriteContentFile).not.toHaveBeenCalled();
  });
});
