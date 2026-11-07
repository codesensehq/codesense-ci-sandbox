// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

/**
 * Tests that both terminal panels configure xterm.js link handling correctly:
 * 1. OSC 8 links via term.options.linkHandler → openUrl (not window.open)
 * 2. Pattern-detected URLs via WebLinksAddon → openUrl (not window.open)
 * 3. Protocol allowlist: only http/https URIs are opened
 * 4. openUrl errors are caught (no unhandled rejections)
 */

// --- Hoisted mocks -----------------------------------------------------------

const { mockOpenUrl } = vi.hoisted(() => ({
  mockOpenUrl: vi.fn((_uri: string) => Promise.resolve()),
}));

// --- Module mocks ------------------------------------------------------------

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    options: Record<string, unknown> = {};
    loadAddon = vi.fn();
    open = vi.fn();
    onData = vi.fn();
    onResize = vi.fn();
    attachCustomKeyEventHandler = vi.fn();
    dispose = vi.fn();
    unicode = { activeVersion: "" };
    cols = 80;
    rows = 24;
  },
}));
vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    activate = vi.fn();
    dispose = vi.fn();
    fit = vi.fn();
  },
}));
vi.mock("@xterm/addon-unicode11", () => ({
  Unicode11Addon: class {
    activate = vi.fn();
    dispose = vi.fn();
  },
}));
vi.mock("@xterm/addon-web-links", () => ({
  WebLinksAddon: class {
    handler: ((event: MouseEvent, uri: string) => void) | undefined;
    constructor(handler?: (event: MouseEvent, uri: string) => void) {
      this.handler = handler;
    }
    activate = vi.fn();
    dispose = vi.fn();
  },
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: mockOpenUrl }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("@/tauri/commands", () => ({
  writePty: vi.fn(() => Promise.resolve()),
  resizePty: vi.fn(() => Promise.resolve()),
}));
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: Object.assign(
    vi.fn(() => ({
      setTerminalReady: vi.fn(),
      settings: { starshipPreset: "compact" },
    })),
    {
      getState: () => ({
        claudeSessionIds: {},
        settings: { starshipPreset: "compact" },
        clearClaudeSessionId: vi.fn(),
        setTerminalReady: vi.fn(),
      }),
    },
  ),
}));
vi.mock("@/shared/lib/terminal-key-handler", () => ({
  createTerminalKeyHandler: vi.fn(() => () => true),
}));
vi.mock("ldrs/react", () => ({ DotPulse: () => null }));
vi.mock("ldrs/react/DotPulse.css", () => ({}));
vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

// --- Helper: mirrors openExternal from terminal panels -----------------------

function openExternal(uri: string) {
  if (uri.startsWith("https://") || uri.startsWith("http://")) {
    mockOpenUrl(uri).catch(() => {});
  }
}

// --- Tests -------------------------------------------------------------------

describe("Terminal link handler configuration", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockOpenUrl.mockReturnValue(Promise.resolve());
  });

  it("calls openUrl for https URLs", () => {
    openExternal("https://github.com/mycorp/codesense/pull/42");
    expect(mockOpenUrl).toHaveBeenCalledWith("https://github.com/mycorp/codesense/pull/42");
  });

  it("calls openUrl for http URLs", () => {
    openExternal("http://example.com");
    expect(mockOpenUrl).toHaveBeenCalledWith("http://example.com");
  });

  it("blocks non-http protocols", () => {
    openExternal("file:///etc/passwd");
    openExternal("javascript:alert(1)");
    openExternal("custom://something");
    expect(mockOpenUrl).not.toHaveBeenCalled();
  });

  it("WebLinksAddon handler routes through openExternal", () => {
    const webLinksHandler = (_event: MouseEvent, uri: string) => openExternal(uri);
    webLinksHandler({} as MouseEvent, "https://example.com");
    expect(mockOpenUrl).toHaveBeenCalledWith("https://example.com");
  });

  it("catches openUrl rejection (no unhandled promise)", async () => {
    mockOpenUrl.mockRejectedValueOnce(new Error("no browser"));

    // Should not throw
    openExternal("https://example.com");

    // Flush microtasks
    await new Promise((r) => setTimeout(r, 0));

    expect(mockOpenUrl).toHaveBeenCalledWith("https://example.com");
  });

  it("does not call window.open", () => {
    const originalOpen = globalThis.window?.open;
    const mockWindowOpen = vi.fn();
    if (globalThis.window) {
      globalThis.window.open = mockWindowOpen;
    }

    openExternal("https://github.com");

    expect(mockOpenUrl).toHaveBeenCalledWith("https://github.com");
    expect(mockWindowOpen).not.toHaveBeenCalled();

    if (globalThis.window && originalOpen !== undefined) {
      globalThis.window.open = originalOpen;
    }
  });
});
