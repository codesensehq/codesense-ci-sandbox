// SPDX-License-Identifier: MIT
import { shimSource } from "../../../../../tests/support/page-shim";
vi.mock("@/features/dashboard/lib/verb-dispatch", async () =>
  (await import("@/test/verb-dispatch-mock")).openVerbDispatchMock(),
);
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { frameIpc, frameEventClass } from "../../../../../tests/support/fixtures/frame";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { Window as DocWindow } from "happy-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Seam: a pane runs behind the REAL `window.codesense` shim (lifted from viewer.rs)
// and talks over postMessage to the REAL right-pane host, pane body, HtmlViewer,
// bridge, action gate, stores and Tauri command wrappers. Only the Tauri IPC and
// its events are stubbed, standing in for the backend (whose path validation and
// byte-preserving write have their own Rust tests).
const { tauriInvoke, listeners } = vi.hoisted(() => ({
  tauriInvoke: vi.fn(),
  listeners: new Map<string, Set<(e: { payload: unknown }) => void>>(),
}));
vi.mock("@tauri-apps/api/core", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@tauri-apps/api/core")>();
  return { ...actual, invoke: (cmd: string, args?: unknown) => tauriInvoke(cmd, args) };
});
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, cb: (e: { payload: unknown }) => void) => {
    const set = listeners.get(event) ?? new Set();
    set.add(cb);
    listeners.set(event, set);
    return () => set.delete(cb);
  }),
  emit: vi.fn(async () => {}),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn(async () => {}) }));
vi.mock("@/features/viewer/components/scratch-note-panel", () => ({
  ScratchNotePanel: () => <div data-testid="scratch-note-panel" />,
}));

import { TooltipProvider } from "@/components/ui/tooltip";
import { RightPaneLayout } from "@/features/viewer/components/right-pane-layout";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { PaneListing } from "@/tauri/commands";
import { CONTEXT_INDEX_CHANGED_EVENT } from "@/tauri/events";
import type { Org } from "@/shared/types/domain";

import { getPanel } from "../panel-registry";
import { defaultRightPaneLayout } from "../right-pane-layout";
import { PanePanelsMount } from "./use-register-pane-panels";

const PORT = 9322;
const PaneMessageEvent = frameEventClass(PORT);
const ORIGIN = `http://f-test.localhost:${PORT}`;
const ORG = "acme/api";
const TEAM = "backend";
const ROOT = "/h/users/42/orgs/acme/api";
const CONTENT = `${ROOT}/${TEAM}/content`;
const WEB = `${ROOT}/web/content`;
const PANEL = `pane:${ORG}:${TEAM}/board`;
const STORY = `${CONTENT}/e/SW-1.md`;
const WEB_STORY = `${WEB}/e/WEB-1.md`;

const SHIM = shimSource();

// A board pane: it scopes its live query to its own team via `context()`.
const PANE = `<!doctype html><html><head><title>Board</title></head><body>
<p id="where">…</p><p id="points">…</p><p id="runs">0</p>
<script>
  var runs = 0;
  codesense.context().then(function (c) {
    document.getElementById("where").textContent = c ? c.org + "/" + c.team : "none";
    codesense.subscribeContext(
      { where: { orgs: [c.org], teams: [c.team], layers: ["team"], kinds: ["story"] } },
      function (r) {
        runs++;
        document.getElementById("runs").textContent = String(runs);
        document.getElementById("points").textContent = String(r.rows[0].fields.points);
      },
    );
  });
</script></body></html>`;

/** The story's frontmatter as the stubbed backend holds it. */
let fields: Record<string, unknown> = {};

function listing(): PaneListing {
  return {
    contentRoot: CONTENT,
    chain: [TEAM],
    panes: [
      {
        id: "board",
        scope: "team",
        team: TEAM,
        title: "Board",
        path: `${ROOT}/${TEAM}/panes/board.html`,
        panesDir: `${ROOT}/${TEAM}/panes`,
        panelId: PANEL,
      },
    ],
  };
}

function emitContextChanged() {
  for (const cb of listeners.get(CONTEXT_INDEX_CHANGED_EVENT) ?? [])
    cb({ payload: { orgs: [ORG], allOrgs: false, truncated: false } });
}

function stubTauri(cmd: string, args: Record<string, unknown> | undefined): unknown {
  const frameResult = frameIpc(cmd, args, PORT);
  if (frameResult !== undefined) return frameResult;
  switch (cmd) {
    case "get_http_api_port":
      return PORT;
    case "list_panes":
      return listing();
    case "bridge_doc_context": {
      const page = String(args?.page ?? "");
      return page.startsWith(`${CONTENT}/`)
        ? { org: ORG, team: TEAM, layer: "team", contentRoot: CONTENT }
        : null;
    }
    case "bridge_resolve_open_target": {
      const path = String(args?.path ?? "");
      if (args?.team === "web" && path === "e/WEB-1.md")
        return { path: WEB_STORY, org: ORG, team: "web", contentRoot: WEB };
      if (path === STORY) return { path: STORY, org: ORG, team: TEAM, contentRoot: CONTENT };
      throw new Error(`refused: ${path} is not inside a known content root`);
    }
    case "update_frontmatter": {
      // The real command writes the file, then the index re-reads it and emits.
      const write = args?.fields as Record<string, unknown>;
      for (const [k, v] of Object.entries(write)) {
        if (v === null) delete fields[k];
        else fields[k] = v;
      }
      setTimeout(emitContextChanged);
      return null;
    }
    case "query_context":
      return {
        schemaVersion: 1,
        total: 1,
        rows: [
          {
            id: "SW-1",
            path: STORY,
            relPath: "e/SW-1.md",
            fileName: "SW-1.md",
            org: ORG,
            team: TEAM,
            layer: "team",
            kind: "story",
            mtime: 0,
            title: "SW-1",
            fields: { ...fields },
          },
        ],
        scopeOrgs: [ORG],
      };
    default:
      return null;
  }
}

interface PaneDoc {
  win: DocWindow & { codesense: Record<string, (...a: unknown[]) => Promise<unknown>> };
  doc: Document;
}

/** Run `html` behind the real shim as the document of the pane's iframe. */
function bootPaneDoc(frame: HTMLIFrameElement, html: string): PaneDoc {
  const win = new DocWindow({
    url: frame.getAttribute("src") ?? ORIGIN,
    settings: {
      enableJavaScriptEvaluation: true,
      suppressInsecureJavaScriptEnvironmentWarning: true,
    } as never,
  });
  const clone = (data: unknown) => JSON.parse(JSON.stringify(data)) as unknown;
  const parentProxy = {
    postMessage: (data: unknown) =>
      setTimeout(() =>
        window.dispatchEvent(
          new PaneMessageEvent("message", {
            data: clone(data),
            source: childProxy as unknown as Window,
            origin: ORIGIN,
          }),
        ),
      ),
  };
  const childProxy = {
    postMessage: (data: unknown) =>
      setTimeout(() =>
        win.dispatchEvent(
          new win.MessageEvent("message", {
            data: clone(data),
            source: parentProxy as never,
          }) as never,
        ),
      ),
  };
  Object.defineProperty(frame, "contentWindow", { value: childProxy, configurable: true });
  (win as unknown as { parent: unknown }).parent = parentProxy;
  win.document.write(
    `<script>window.__codesenseProxy={token:"frame-token"}</script><script>${SHIM}</script>${html}`,
  );
  return { win: win as PaneDoc["win"], doc: win.document as unknown as Document };
}

const onFileSelect = vi.fn();
const mockFetch = vi.fn();
globalThis.fetch = mockFetch;

beforeEach(() => {
  liveHub.dispose();
  listeners.clear();
  onFileSelect.mockClear();
  fields = { status: "todo", points: 2 };
  tauriInvoke.mockReset();
  tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) =>
    stubTauri(cmd, args),
  );
  mockFetch.mockImplementation(async () => ({ ok: true, status: 200, statusText: "" }));
  useOrgStore.setState({
    orgs: [{ slug: ORG, teams: [{ name: TEAM }, { name: "web" }] } as unknown as Org],
    activeOrgSlug: ORG,
    activeTeamName: TEAM,
  });
  useViewerStore.setState({
    rightPaneLayout: { sections: defaultRightPaneLayout().sections.slice(1) },
    rightPaneOpen: false,
    openTabs: {},
    activeTab: {},
  });
});

afterEach(() => cleanup());

async function openBoard(): Promise<PaneDoc> {
  render(
    <TooltipProvider>
      <PanePanelsMount />
      <RightPaneLayout repoPath="" onClose={() => {}} onFileSelect={onFileSelect} />
    </TooltipProvider>,
  );
  await waitFor(() => expect(getPanel(PANEL)).toBeDefined());
  await act(async () => useViewerStore.getState().revealPanelById(PANEL));
  const frame = () => {
    const f = screen.getByTestId("pane-body").querySelector("iframe");
    if (!f) throw new Error("no pane iframe");
    return f;
  };
  await waitFor(() => expect(frame().getAttribute("src")).toContain("/pane/"));
  const pane = bootPaneDoc(frame(), PANE);
  await waitFor(() => expect(pane.doc.getElementById("points")?.textContent).toBe("2"));
  return pane;
}

describe("pane bridge: context, openFile, setField (seam)", () => {
  it("context() names the pane's own team from the host's anchor, and scopes its query", async () => {
    const pane = await openBoard();
    expect(pane.doc.getElementById("where")?.textContent).toBe(`${ORG}/${TEAM}`);
    // The host, not the page, supplied the page path: the pane's content-root anchor.
    expect(tauriInvoke).toHaveBeenCalledWith("bridge_doc_context", {
      page: `${CONTENT}/board.html`,
    });
    expect(tauriInvoke).toHaveBeenCalledWith(
      "query_context",
      expect.objectContaining({
        query: { where: { orgs: [ORG], teams: [TEAM], layers: ["team"], kinds: ["story"] } },
      }),
    );
  });

  it("setField writes through update_frontmatter and the index change re-runs subscribeContext", async () => {
    const pane = await openBoard();
    const runs = () => Number(pane.doc.getElementById("runs")?.textContent);
    const before = runs();
    await act(async () => {
      await pane.win.codesense.setField({ path: STORY, field: "points", value: 5 });
    });
    expect(tauriInvoke).toHaveBeenCalledWith("update_frontmatter", {
      path: STORY,
      fields: { points: 5 },
      frameId: "test",
      pagePath: `${CONTENT}/board.html`,
    });
    await waitFor(() => expect(pane.doc.getElementById("points")?.textContent).toBe("5"));
    expect(runs()).toBeGreaterThan(before);

    // null removes the key; a refusal reaches the page as a readable rejection.
    await act(async () => {
      await pane.win.codesense.setField({ path: STORY, field: "points", value: null });
    });
    await waitFor(() => expect(pane.doc.getElementById("points")?.textContent).toBe("undefined"));
    tauriInvoke.mockImplementationOnce(async () => {
      throw new Error(
        "refusing to edit metadata: /etc/x.md is not inside a team or private content root",
      );
    });
    let error = "";
    await act(async () => {
      await pane.win.codesense
        .setField({ path: "/etc/x.md", field: "points", value: 1 })
        .catch((e: Error) => (error = e.message));
    });
    expect(error).toMatch(/^refusing to edit metadata/);
  });

  it("script openFile opens in the pane host or another team's memory without switching", async () => {
    const pane = await openBoard();
    useViewerStore.setState({
      layoutMode: "console",
      terminalVisible: true,
      contextHistory: { back: [], forward: [] },
    });
    await act(async () => {
      await pane.win.codesense.openFile({ path: STORY, newTab: true });
    });
    expect(tauriInvoke).toHaveBeenCalledWith("bridge_resolve_open_target", {
      frameId: "test",
      page: `${CONTENT}/board.html`,
      path: STORY,
      team: null,
    });
    // A pane script's open is not the user's: it keeps the CLI's layout.
    expect(onFileSelect).toHaveBeenCalledWith(STORY, { openInNewTab: true, keepLayout: true });

    await act(async () => {
      await pane.win.codesense.openFile("e/WEB-1.md", { newTab: false }).catch(() => {});
    });
    // A relative path without a team resolves in the pane's own root: not found there.
    expect(useOrgStore.getState().activeTeamName).toBe(TEAM);

    await act(async () => {
      await pane.win.codesense.openFile({ team: "web", path: "e/WEB-1.md" });
    });
    expect(useOrgStore.getState().activeTeamName).toBe(TEAM);
    expect(useViewerStore.getState().layoutMode).toBe("console");
    expect(useViewerStore.getState().contextHistory).toEqual({ back: [], forward: [] });
    const webKey = buildTeamKey(ORG, "web");
    expect(useViewerStore.getState().openTabs[webKey]?.map((t) => t.path)).toEqual([WEB_STORY]);
    expect(useViewerStore.getState().activeTab[webKey]).toBe(WEB_STORY);
  });

  it("openFile refuses a path outside every team content root and opens nothing", async () => {
    const pane = await openBoard();
    let error = "";
    await act(async () => {
      await pane.win.codesense.openFile("/etc/hosts").catch((e: Error) => (error = e.message));
    });
    expect(error).toMatch(/^refused:/);
    expect(onFileSelect).not.toHaveBeenCalled();
    expect(useOrgStore.getState().activeTeamName).toBe(TEAM);
    expect(Object.values(useViewerStore.getState().openTabs ?? {}).flat()).toEqual([]);
  });
});
