// SPDX-License-Identifier: MIT
vi.mock("@/features/dashboard/lib/verb-dispatch", async () =>
  (await import("@/test/verb-dispatch-mock")).openVerbDispatchMock(),
);
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));
import { frameFixture, FrameMessageEvent } from "../../../../tests/support/fixtures/frame";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, cleanup } from "@testing-library/react";

// Seam: the REAL HtmlViewer, bridge, action dispatch and dashboard-launch
// controller, the real viewer and org stores, and the real Tauri command
// wrappers — only the Tauri IPC itself is stubbed. A doc with no meta and no
// prior state must get the read capability and a serviced bridge whatever the
// org situation is, and nothing may ask the user anything.
const { tauriInvoke } = vi.hoisted(() => ({ tauriInvoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tauri-apps/api/core")>();
  return { ...actual, invoke: (cmd: string, args?: unknown) => tauriInvoke(cmd, args) };
});
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
  emit: vi.fn(async () => {}),
}));

import { HtmlViewer } from "./html-viewer";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

const PORT = 9321;
const DOC = "/ctx/acme/default/content/dashboards/board.html";
const DOC_HTML = "<!doctype html><html><head></head><body>board</body></html>";
const DIR = "/work/some-dir";

function stubTauri(cmd: string, args?: unknown): unknown {
  switch (cmd) {
    case "get_http_api_port":
      return PORT;
    case "mint_frame":
      return frameFixture((args as { args: import("@/tauri/commands").MintFrameArgs }).args, PORT);
    case "release_frame":
      return null;
    case "resolve_frame_grant":
      return frameFixture(
        { documentPath: DOC, contentRoot: "/ctx/acme/default/content", slot: "centre" },
        PORT,
      ).grant;
    case "read_file":
      return DOC_HTML;
    case "dashboard_probe_sessions_alive":
      return [];
    default:
      return null;
  }
}

const mockFetch = vi.fn();
globalThis.fetch = mockFetch;

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  tauriInvoke.mockImplementation(async (cmd: string, args?: unknown) => stubTauri(cmd, args));
  mockFetch.mockResolvedValue({ status: 200 });
});

afterEach(() => {
  cleanup();
});

type Scenario = { name: string; setup: () => void };

const scenarios: Scenario[] = [
  {
    name: "org unknown (no active org)",
    setup: () => useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null }),
  },
  {
    name: "org never trusted (real team, no prior decision)",
    setup: () => useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" }),
  },
  {
    name: "empty localStorage (org unrelated to the doc)",
    setup: () => {
      localStorage.clear();
      useOrgStore.setState({ activeOrgSlug: "elsewhere", activeTeamName: "other" });
    },
  },
];

async function mountDoc() {
  render(<HtmlViewer filePath={DOC} teamContentPath="/ctx/acme/default/content" />);
  await waitFor(() => expect(document.querySelector("iframe")?.getAttribute("src")).toBeTruthy());
  const iframe = document.querySelector("iframe")!;
  const postMessage = vi.fn();
  const fakeWindow = { postMessage } as unknown as Window;
  Object.defineProperty(iframe, "contentWindow", {
    value: fakeWindow,
    writable: true,
    configurable: true,
  });
  let nextId = 0;
  const call = async (command: string, args: unknown) => {
    const id = ++nextId;
    window.dispatchEvent(
      new FrameMessageEvent("message", {
        origin: `http://f-test.localhost:${PORT}`,
        data: { type: "codesense:invoke", id, command, args },
        source: fakeWindow,
      }),
    );
    await waitFor(() => expect(postMessage.mock.calls.some(([msg]) => msg?.id === id)).toBe(true));
    const [reply, origin] = postMessage.mock.calls.find(([msg]) => msg?.id === id)!;
    return { reply, origin };
  };
  return { iframe, call };
}

describe("HtmlViewer services every doc with no grants, trust or confirms", () => {
  for (const scenario of scenarios) {
    it(`${scenario.name}: query, getWorkspaceStatus and openWorkspace are all serviced`, async () => {
      scenario.setup();
      const { iframe, call } = await mountDoc();

      // The read API uses the credential injected for this frame, so its URL
      // carries only its public origin and document location.
      const src = new URL(iframe.getAttribute("src")!);
      expect(src.searchParams.has("cap")).toBe(false);
      expect(iframe.getAttribute("sandbox")).toContain("allow-same-origin");

      const status = await call("getWorkspaceStatus", { storyId: "SW-1" });
      expect(status.reply).toEqual({
        type: "codesense:invoke-result",
        id: expect.any(Number),
        ok: true,
        data: { exists: false, live: false, sessionId: null },
      });
      expect(status.origin).toBe(`http://f-test.localhost:${PORT}`);

      const opened = await call("openWorkspace", { target: { kind: "dir", path: DIR } });
      expect(opened.reply.ok).toBe(true);
      const workspaces = Object.values(useViewerStore.getState().workspaces).flat();
      expect(workspaces.some((ws) => ws.kind === "generic" && ws.cwd === DIR)).toBe(true);

      expect(screen.queryByRole("dialog")).toBeNull();
      expect(screen.queryByRole("alertdialog")).toBeNull();
      // The doc's own content is never read to look for a capability manifest.
      expect(tauriInvoke.mock.calls.some(([cmd]) => cmd === "read_file")).toBe(false);
    });
  }
});
