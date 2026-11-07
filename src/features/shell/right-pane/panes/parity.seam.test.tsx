// SPDX-License-Identifier: MIT
import { shimSource } from "../../../../../tests/support/page-shim";
vi.mock("@/features/dashboard/lib/verb-dispatch", async () =>
  (await import("@/test/verb-dispatch-mock")).openVerbDispatchMock(),
);
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { themeTokens } from "@/themes";
import { applyTheme } from "@/themes/runtime";
import { frameIpc, frameEventClass } from "../../../../../tests/support/fixtures/frame";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { Window as DocWindow } from "happy-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Seam: the parity fixture runs as a real pane. Its HTML and script execute in
// their own window behind the REAL `window.codesense` shim (lifted from
// viewer.rs, as the app injects it), and talk over postMessage to the REAL
// right-pane host, stack, pane registration, pane body, HtmlViewer, bridge,
// stores and Tauri command wrappers. Only the Tauri IPC and its events are
// stubbed, standing in for the backend.
const { tauriInvoke, listeners, openUrl } = vi.hoisted(() => ({
  tauriInvoke: vi.fn(),
  listeners: new Map<string, Set<(e: { payload: unknown }) => void>>(),
  openUrl: vi.fn(async () => {}),
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
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl }));
// The terminal host a launched agent's tab would open in.
vi.mock("@/shared/lib/agent-session-launcher", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/agent-session-launcher")>()),
  waitForAgentLive: vi.fn(async () => true),
  makeRealLaunchDeps: () => ({
    findExisting: async () => undefined,
    provision: async () => undefined,
    openTab: async () => undefined,
    closeTab: () => undefined,
    launchAgentLine: async () => ({ ok: true }),
    projectScopeConfig: async () => ({ extraArgs: [], environmentBinding: null }),
  }),
}));
vi.mock("@/features/viewer/components/scratch-note-panel", () => ({
  ScratchNotePanel: () => <div data-testid="scratch-note-panel" />,
}));

import { TooltipProvider } from "@/components/ui/tooltip";
import { RightPaneLayout } from "@/features/viewer/components/right-pane-layout";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type {
  AgentSessionRow,
  FrameGrant,
  FrameRecord,
  PaneEntry,
  PaneListing,
} from "@/tauri/commands";
import type { Org } from "@/shared/types/domain";

import { buildPanelMenuEntries } from "../panel-menu";
import { getPanel } from "../panel-registry";
import {
  defaultRightPaneLayout,
  findPanelLocation,
  reconcileLayoutToBuiltins,
  SCRATCH_PANEL_ID,
  type RightPaneSection,
} from "../right-pane-layout";
import { PanePanelsMount } from "./use-register-pane-panels";

const PORT = 9321;
const PaneMessageEvent = frameEventClass(PORT);
const ORIGIN = `http://f-test.localhost:${PORT}`;
const ORG = "acme/api";
const TEAM = "backend";
const ROOT = "/h/users/42/orgs/acme/api";
const CONTENT = `${ROOT}/${TEAM}/content`;
const PARITY = `pane:${ORG}:${TEAM}/parity`;
const REPO = `workspace:${ORG}:${TEAM}:acme/api`;

const FIXTURE = readFileSync(resolve(__dirname, "../__fixtures__/parity-pane.html"), "utf8");
const WORKER = readFileSync(resolve(__dirname, "../__fixtures__/parity-worker.js"), "utf8");
const SHIM = shimSource();

/** Panes on disk, `${id}` → html, all in the active team's panes/. */
const disk = new Map<string, string>();

function listing(): PaneListing {
  const panes: PaneEntry[] = [...disk].map(([id, html]) => ({
    id,
    scope: "team",
    team: TEAM,
    title: /<title>(.*?)<\/title>/.exec(html)?.[1] ?? id,
    ...(/name="codesense:icon" content="([^"]+)"/.exec(html)
      ? { icon: /name="codesense:icon" content="([^"]+)"/.exec(html)![1] }
      : {}),
    path: `${ROOT}/${TEAM}/panes/${id}.html`,
    panesDir: `${ROOT}/${TEAM}/panes`,
    panelId: `pane:${ORG}:${TEAM}/${id}`,
  }));
  return { contentRoot: CONTENT, chain: [TEAM], panes };
}

/** Context files the query engine knows: two stories and an epic. */
const CONTEXT_ROWS = [
  { id: "SW-1", kind: "story", status: "review" },
  { id: "SW-2", kind: "story", status: "todo" },
  { id: "SW", kind: "epic", status: "in-progress" },
].map((r) => ({
  id: r.id,
  path: `${CONTENT}/${r.id}.md`,
  relPath: `${r.id}.md`,
  fileName: `${r.id}.md`,
  org: ORG,
  team: TEAM,
  layer: "team",
  kind: r.kind,
  mtime: 0,
  title: r.id,
  fields: { status: r.status },
}));

let sessions: AgentSessionRow[] = [];
/** The state `list_sessions` reports per handle (Rust derives it from the session's hooks). */
let listedState: Record<string, string> = {};
let queryFails = false;

const AGENT_DEFINITIONS = [
  {
    key: "builtin/claude",
    name: "Claude",
    description: "Claude Code",
    runtime: "claude",
    scope: "builtin",
    team: "",
    model: null,
    running_now: 1,
  },
];

/** What the backend's agent-definitions topic holds: one `definition` row per agent. */
function agentDefinitionsView() {
  return {
    topic: "agentDefinitions",
    viewKey: "agents",
    rev: 0,
    snapshot: AGENT_DEFINITIONS.map((row) => ({ id: row.key, rowType: "definition", row })),
  };
}

function stubTauri(cmd: string, args: Record<string, unknown> | undefined): unknown {
  if (cmd === "mint_frame") {
    return Promise.resolve(frameIpc(cmd, args, PORT)).then((value) => {
      const frame = value as FrameRecord;
      return { ...frame, grant: parityGrant(frame.grant) };
    });
  }
  if (cmd === "resolve_frame_grant") {
    return Promise.resolve(frameIpc(cmd, args, PORT)).then((value) =>
      parityGrant(value as FrameGrant),
    );
  }
  const frameResult = frameIpc(cmd, args, PORT);
  if (frameResult !== undefined) return frameResult;
  switch (cmd) {
    case "live_hub_unsubscribe":
      return null;
    case "bridge_doc_context":
      return { org: ORG, team: TEAM, layer: "team", contentRoot: CONTENT };
    case "get_http_api_port":
      return PORT;
    case "list_panes":
      return listing();
    case "query_context": {
      if (queryFails) throw new Error("index rebuilding");
      const q = (args?.query ?? {}) as { where?: { kinds?: string[] } };
      const rows = CONTEXT_ROWS.filter((r) => !q.where?.kinds || q.where.kinds.includes(r.kind));
      return { schemaVersion: 1, total: rows.length, rows, scopeOrgs: [ORG] };
    }
    case "wv_list_roster":
      return [];
    case "dashboard_list_sessions":
      return sessions.map((s) => ({ ...s }));
    // The agent definitions the app reads through the hub: prepared once, then subscribed.
    case "agent_definitions_prepare":
      return agentDefinitionsView();
    case "live_hub_subscribe":
      if (args?.topic === "agentDefinitions") return agentDefinitionsView();
      return {
        topic: args?.topic,
        viewKey: args?.viewKey,
        rev: 0,
        snapshot:
          args?.topic === "inbox"
            ? []
            : (stubTauri("agent_read_sessions", {}) as Record<string, unknown>[]).map((row) => ({
                ...sessions.find((session) => session.handle === row.handle),
                id: row.handle,
                ...row,
              })),
      };
    case "agent_read_sessions":
      return sessions.map((s) => ({
        ...s,
        handle: s.handle,
        displayName: s.displayName ?? s.handle,
        state: listedState[s.handle] ?? "running",
        launchedBy: null,
        unread: 0,
        oldestUnreadAt: null,
        machine: "This Mac",
        ...(s.agentRevision ? { agent: s.agentRevision.replace(/@[^@]*$/, "") } : {}),
      }));
    case "dashboard_kill_by_handle":
      sessions = sessions.filter((s) => s.handle !== args?.handle);
      for (const callback of listeners.get("codesense:delta") ?? [])
        callback({
          payload: {
            topic: "sessions",
            viewKey: "all",
            rev: 2,
            upserts: [],
            removes: [args?.handle],
          },
        });
      return null;
    case "list_attention_board":
      return { youOwe: [], owedToYou: [], onAgent: [], parked: [], doneUnacked: [] };
    case "wv_list_pending_approvals":
      return [];
    case "dashboard_probe_sessions_alive":
      return [];
    case "dashboard_launch_begin":
      return true;
    case "wv_roster_for_session":
      return { status: "dispatch" };
    default:
      return null;
  }
}

/** A right-pane parity fixture is a team page, so its frame needs that caller scope. */
function parityGrant(grant: FrameGrant): FrameGrant {
  return {
    ...grant,
    dashboard: PARITY,
    team: { org: ORG, team: TEAM },
    scope: "team",
  };
}

/** The app's theme as the stylesheet resolves it where the pane's frame sits. */
const LIGHT = { "--cs-viewer-bg": "#ffffff", color: "#1f2328", "--border": "#d0d7de" };
const DARK = { "--cs-viewer-bg": "#282a36", color: "#f8f8f2", "--border": "#44475a" };

interface PaneDoc {
  win: DocWindow;
  doc: Document;
}

const docs: PaneDoc[] = [];

/** Run `html` behind the real shim as the document of the pane's iframe. */
function bootPaneDoc(frame: HTMLIFrameElement, html: string): PaneDoc {
  const win = new DocWindow({
    url: `${frame.getAttribute("src")}#poll=40`,
    settings: {
      enableJavaScriptEvaluation: true,
      suppressInsecureJavaScriptEnvironmentWarning: true,
    } as never,
  });
  const clone = (data: unknown) => JSON.parse(JSON.stringify(data)) as unknown;
  // The doc's view of its parent, and the host's view of the doc's window.
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
            origin: "tauri://localhost",
          }) as never,
        ),
      ),
  };
  Object.defineProperty(frame, "contentWindow", { value: childProxy, configurable: true });
  (win as unknown as { parent: unknown }).parent = parentProxy;
  win.document.write(
    `<script>window.__codesenseProxy={token:"frame-token"}</script><script>${SHIM}</script>${html}`,
  );
  const doc = { win, doc: win.document as unknown as Document };
  docs.push(doc);
  return doc;
}

function paneFrame(): HTMLIFrameElement {
  const frame = screen.getByTestId("pane-body").querySelector("iframe");
  if (!frame) throw new Error("no pane iframe");
  return frame;
}

const mockFetch = vi.fn();
globalThis.fetch = mockFetch;
let computedStyle: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  liveHub.dispose();
  disk.clear();
  listeners.clear();
  queryFails = false;
  listedState = {};
  sessions = [
    {
      handle: "SW-1",
      sessionId: "pty-1",
      kind: "dashboard-launched",
      alive: true,
      status: "running",
      backgrounded: false,
      startedWith: null,
      startedAtMs: 1,
      launcher: null,
      lastOutputMs: null,
      agentRevision: "builtin/claude@3",
      story: "SW-1",
      displayName: "SW-1 dev",
    },
  ];
  tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) =>
    stubTauri(cmd, args),
  );
  mockFetch.mockImplementation(async () => ({ ok: true, status: 200, statusText: "" }));
  openUrl.mockClear();
  useAgentListProjectionStore.setState({ sessions: null, error: null });
  useManagedSessionStore.setState({ states: {} });
  useOrgStore.setState({
    orgs: [{ slug: ORG } as unknown as Org],
    activeOrgSlug: ORG,
    activeTeamName: TEAM,
  });
  useViewerStore.setState({
    rightPaneLayout: { sections: defaultRightPaneLayout().sections.slice(1) },
    rightPaneOpen: false,
  });
  document.documentElement.classList.remove("dark");
  document.documentElement.removeAttribute("style");
  document.documentElement.removeAttribute("data-surface");
  document.documentElement.removeAttribute("data-app-theme");
  document.getElementById("app-theme-values")?.remove();
  // The stylesheet's cascade, resolved for the pane's frame: light or dark by the
  // app's class, as the real stylesheet does.
  const real = window.getComputedStyle.bind(window);
  computedStyle = vi.spyOn(window, "getComputedStyle").mockImplementation((el, pseudo) => {
    const style = real(el, pseudo);
    if (!(el instanceof HTMLIFrameElement)) return style;
    const theme = document.documentElement.classList.contains("dark") ? DARK : LIGHT;
    return {
      ...style,
      getPropertyValue: (prop: string) =>
        (theme as Record<string, string>)[prop] ?? style.getPropertyValue(prop),
    } as CSSStyleDeclaration;
  });
});

afterEach(async () => {
  cleanup();
  for (const doc of docs.splice(0)) await doc.win.happyDOM.close();
  computedStyle.mockRestore();
});

/** Mount the right pane, write the fixture as panes/parity.html, open it and boot it. */
async function openParityPane(): Promise<PaneDoc> {
  disk.set("parity", FIXTURE);
  render(
    <TooltipProvider>
      <PanePanelsMount />
      <RightPaneLayout repoPath={REPO} onClose={() => {}} />
    </TooltipProvider>,
  );
  await waitFor(() => expect(getPanel(PARITY)).toBeDefined());
  await act(async () => useViewerStore.getState().revealPanelById(PARITY));
  await waitFor(() => expect(paneFrame().getAttribute("src")).toContain("/pane/"));
  const pane = bootPaneDoc(paneFrame(), FIXTURE);
  await waitFor(() => expect(pane.doc.getElementById("query-state")?.textContent).toBe("ready"));
  return pane;
}

const byId = (pane: PaneDoc, id: string) => pane.doc.getElementById(id)!;

describe("HTML panes reach parity with JSON widgets (seam)", () => {
  it("P1/P3/P4/P7/P16: a live story query renders stat, table, chips and a template; stale on error", async () => {
    const pane = await openParityPane();
    // P4: the query asked for stories only, with no row limit in the way.
    expect(tauriInvoke).toHaveBeenCalledWith(
      "query_context",
      expect.objectContaining({
        query: expect.objectContaining({ where: { kinds: ["story"] }, limit: 10000 }),
      }),
    );
    const rows = [...pane.doc.querySelectorAll("#stories tr")];
    expect(rows.map((r) => r.getAttribute("data-kind"))).toEqual(["story", "story"]);
    expect(rows[0].textContent).toContain("SW-1");
    expect(rows[0].querySelector(".chip")?.textContent).toBe("review");
    expect(byId(pane, "story-count").textContent).toBe("2");
    // P7: `{{first.id}} is {{first.status}}`.
    expect(byId(pane, "summary").textContent).toBe("SW-1 is review");

    // P3/P16: a context change re-runs the query live; a failure shows the stale cue.
    queryFails = true;
    await act(async () => {
      for (const cb of listeners.get("context-index-changed") ?? [])
        cb({ payload: { orgs: [ORG], allOrgs: false, truncated: false } });
    });
    await waitFor(() =>
      expect(byId(pane, "query-state").textContent).toBe("stale: index rebuilding"),
    );
    expect(byId(pane, "query-state").className).toContain("stale");
  });

  it("P2/P6/P8: an external link opens in the browser; a confirmed row's setStatus reaches Tauri", async () => {
    const pane = await openParityPane();
    (byId(pane, "external") as HTMLElement).click();
    await waitFor(() =>
      expect(openUrl).toHaveBeenCalledWith("https://github.com/alderco/codesense"),
    );

    // P8: the picker walks unset → inferred → confirmed; only then may the row act.
    const review = () => pane.doc.querySelector<HTMLButtonElement>('[data-review="SW-1"]')!;
    expect(review().disabled).toBe(true);
    pane.doc.querySelector<HTMLButtonElement>('[data-trust="SW-1"]')!.click();
    pane.doc.querySelector<HTMLButtonElement>('[data-trust="SW-1"]')!.click();
    expect(pane.doc.querySelector('[data-trust="SW-1"]')?.textContent).toBe("confirmed");
    review().click();
    await waitFor(() =>
      expect(tauriInvoke).toHaveBeenCalledWith("apply_metadata_edit", {
        path: `${CONTENT}/SW-1.md`,
        edits: [{ kind: "setField", field: "status", value: "review" }],
        frameId: "test",
        pagePath: `${CONTENT}/parity.html`,
      }),
    );
    await waitFor(() => expect(review().textContent).toBe("sent"));
  });

  it("P5/P20: polling runs while the pane is on screen and pauses while hidden", async () => {
    const pane = await openParityPane();
    const polls = () => Number(byId(pane, "poll-count").textContent);
    await waitFor(() => expect(polls()).toBeGreaterThan(1));
    expect(byId(pane, "poll-state").textContent).toBe("polling");

    // Closing the right pane hides it: the pane hears it and stops polling.
    await act(async () => useViewerStore.setState({ rightPaneOpen: false }));
    await waitFor(() => expect(byId(pane, "poll-state").textContent).toBe("paused (hidden)"));
    const paused = polls();
    await new Promise((r) => setTimeout(r, 200));
    expect(polls()).toBe(paused);

    // Back on screen, it polls again.
    await act(async () => useViewerStore.setState({ rightPaneOpen: true }));
    await waitFor(() => expect(polls()).toBeGreaterThan(paused));

    // Another tab of its group in front hides it; back in front, it polls again.
    const setSection = (patch: (s: RightPaneSection) => RightPaneSection) =>
      act(async () => {
        const layout = useViewerStore.getState().rightPaneLayout;
        useViewerStore.getState().setRightPaneLayout({
          sections: layout.sections.map((s) => (s.group.panelIds.includes(PARITY) ? patch(s) : s)),
        });
      });
    // Notes moves into the pane's group, in front.
    await act(async () => {
      const layout = useViewerStore.getState().rightPaneLayout;
      useViewerStore.getState().setRightPaneLayout({
        sections: layout.sections
          .map((s) => ({
            ...s,
            group: s.group.panelIds.includes(PARITY)
              ? {
                  panelIds: [...s.group.panelIds, SCRATCH_PANEL_ID],
                  activePanelId: SCRATCH_PANEL_ID,
                }
              : {
                  panelIds: s.group.panelIds.filter((id) => id !== SCRATCH_PANEL_ID),
                  activePanelId: s.group.activePanelId,
                },
          }))
          .filter((s) => s.group.panelIds.length > 0),
      });
    });
    const placed = useViewerStore.getState().rightPaneLayout;
    expect(findPanelLocation(placed, SCRATCH_PANEL_ID)?.sectionIndex).toBe(
      findPanelLocation(placed, PARITY)?.sectionIndex,
    );
    await waitFor(() => expect(byId(pane, "poll-state").textContent).toBe("paused (hidden)"));
    await setSection((s) => ({ ...s, group: { ...s.group, activePanelId: PARITY } }));
    await waitFor(() => expect(byId(pane, "poll-state").textContent).toBe("polling"));

    // The app window hidden (minimised, another app in front) hides it.
    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    await act(async () => document.dispatchEvent(new Event("visibilitychange")));
    await waitFor(() => expect(byId(pane, "poll-state").textContent).toBe("paused (hidden)"));
    visibility.mockReturnValue("visible");
    await act(async () => document.dispatchEvent(new Event("visibilitychange")));
    await waitFor(() => expect(byId(pane, "poll-state").textContent).toBe("polling"));
    visibility.mockRestore();

    // A collapsed section hides it too.
    await setSection((s) => ({ ...s, collapsed: true }));
    await waitFor(() => expect(byId(pane, "poll-state").textContent).toBe("paused (hidden)"));
  });

  it("P9: a pane may start its own Web Worker from a script beside it in panes/", async () => {
    // happy-dom has no Worker, so this proves what a worker needs from the host:
    // the frame runs scripts with its own origin, and a relative worker script
    // resolves on the same origin, inside the pane's own panes/ dir.
    await openParityPane();
    const sandbox = paneFrame().getAttribute("sandbox")!.split(/\s+/);
    expect(sandbox).toEqual(expect.arrayContaining(["allow-scripts", "allow-same-origin"]));
    const src = new URL(paneFrame().getAttribute("src")!, ORIGIN);
    const worker = new URL("parity-worker.js", src);
    expect(worker.origin).toBe(src.origin);
    expect(worker.pathname).toBe(src.pathname.replace(/[^/]+$/, "parity-worker.js"));
  });

  it("P9: the fixture's worker infers the stories ready for review", async () => {
    // The worker script itself, run against a stand-in worker global.
    const scope: {
      onmessage?: (e: { data: unknown }) => void;
      postMessage: ReturnType<typeof vi.fn>;
    } = { postMessage: vi.fn() };
    new Function("self", WORKER)(scope);
    scope.onmessage!({ data: [{ status: "review" }, { status: "todo" }, { status: "review" }] });
    expect(scope.postMessage).toHaveBeenCalledWith({ ready: 2 });
  });

  it("P10/P11: sessions lists the live roster and stopSession kills one; agents lists builtin/claude and launches it", async () => {
    const pane = await openParityPane();
    await waitFor(() =>
      expect(pane.doc.querySelector('[data-session="SW-1"]')?.textContent).toContain(
        "SW-1 dev · builtin/claude · running",
      ),
    );
    await waitFor(() =>
      expect(pane.doc.querySelector('[data-agent="builtin/claude"]')?.textContent).toContain(
        "Claude",
      ),
    );

    // A session's own signal (done) changes its state: the pane hears it and re-reads.
    listedState["SW-1"] = "idle";
    await act(async () => {
      useManagedSessionStore.setState({ states: { "pty-1": "done" } });
      for (const cb of listeners.get("codesense:delta") ?? [])
        cb({
          payload: {
            topic: "sessions",
            viewKey: "all",
            rev: 1,
            upserts: stubTauri("agent_read_sessions", {}),
            removes: [],
          },
        });
    });
    await waitFor(() =>
      expect(pane.doc.querySelector('[data-session="SW-1"]')?.textContent).toContain(
        "builtin/claude · idle",
      ),
    );

    // An agent from that list launches by name.
    pane.doc.querySelector<HTMLButtonElement>('[data-launch="builtin/claude"]')!.click();
    await waitFor(() =>
      expect(tauriInvoke).toHaveBeenCalledWith(
        "agent_launch_prepare",
        expect.objectContaining({ agentKey: "builtin/claude", cwd: `${ROOT}/${TEAM}/panes` }),
      ),
    );

    pane.doc.querySelector<HTMLButtonElement>('[data-stop="SW-1"]')!.click();
    await waitFor(() =>
      expect(tauriInvoke).toHaveBeenCalledWith("dashboard_kill_by_handle", { handle: "SW-1" }),
    );
    // Like the Agents panel's Stop, a worker still in dispatch is marked first.
    const order = tauriInvoke.mock.calls.map(([cmd]) => cmd);
    expect(order.indexOf("wv_worker_force_close")).toBeGreaterThan(-1);
    expect(order.indexOf("wv_worker_force_close")).toBeLessThan(
      order.indexOf("dashboard_kill_by_handle"),
    );
    // The roster reloads and the pane's onSessions re-runs without the stopped row.
    await waitFor(() => expect(pane.doc.querySelector('[data-session="SW-1"]')).toBeNull());
    expect(byId(pane, "session-count").textContent).toBe("0");
  });

  it("P12/P13/P14/P15: title, icon and badge show in the panel chrome; maximize works", async () => {
    await openParityPane();
    expect(getPanel(PARITY)?.title).toBe("Parity pane");
    await waitFor(() =>
      expect(document.querySelector('[data-pane-icon="git-pull-request"]')).not.toBeNull(),
    );
    // One story is in review: the pane's badge is a warning count of 1.
    await waitFor(() => expect(screen.getByTestId("pane-badge")).toHaveTextContent("1"));
    expect(screen.getByTestId("pane-badge").getAttribute("data-severity")).toBe("warning");

    fireEvent.click(screen.getByRole("button", { name: "Maximize panel" }));
    await waitFor(() =>
      expect(screen.getByTestId(`${PARITY}-fullscreen-shell`).getAttribute("data-fullscreen")).toBe(
        "true",
      ),
    );
    expect(screen.getByTestId("pane-body").querySelector("iframe")).not.toBeNull();
  });

  it("P13: an icon name Lucide doesn't have shows a visible '?' glyph", async () => {
    disk.set("odd", '<title>Odd</title><meta name="codesense:icon" content="NotAnIcon"><p>x</p>');
    await openParityPane();
    const odd = `pane:${ORG}:${TEAM}/odd`;
    await waitFor(() => expect(getPanel(odd)).toBeDefined());
    await act(async () => useViewerStore.getState().revealPanelById(odd));
    await waitFor(() =>
      expect(document.querySelector('[data-pane-icon="unknown"]')).not.toBeNull(),
    );
  });

  it("P17: theme tokens and color-scheme reach the pane, and onTheme fires on a theme toggle", async () => {
    const pane = await openParityPane();
    const root = pane.doc.documentElement;
    await waitFor(() => expect(root.style.getPropertyValue("--cs-bg")).toBe("#ffffff"));
    expect(root.style.getPropertyValue("--cs-fg")).toBe("#1f2328");
    expect(root.style.colorScheme).toBe("light");
    expect(byId(pane, "scheme").textContent).toBe("light");
    expect(paneFrame().style.colorScheme).toBe("light");
    const seen = Number(byId(pane, "theme-changes").textContent);

    // A theme switch is the provider's sequence: apply the tokens and `.dark`, then
    // publish the one revision through the hub (the pane's `onTheme` rides it).
    await act(async () => {
      applyTheme("dracula", "dark");
      liveHub.publishTheme("dracula", "dark");
    });
    // Under Themes v0 the pane's tokens are the switched theme's own surface tokens.
    const dark = themeTokens("dracula", "dark");
    await waitFor(() =>
      expect(root.style.getPropertyValue("--cs-bg")).toBe(dark["--theme-surface-canvas-bg"]),
    );
    expect(root.style.getPropertyValue("--cs-bg")).not.toBe("#ffffff");
    expect(root.style.getPropertyValue("--cs-fg")).toBe(dark["--theme-text-canvas-primary"]);
    expect(root.style.colorScheme).toBe("dark");
    expect(byId(pane, "scheme").textContent).toBe("dark");
    expect(Number(byId(pane, "theme-changes").textContent)).toBe(seen + 1);
    expect(paneFrame().style.colorScheme).toBe("dark");
  });

  it("P18/P19/P21: offered in the Panels menu; the placement survives a layout reload", async () => {
    await openParityPane();
    expect(buildPanelMenuEntries()).toContainEqual({
      id: PARITY,
      label: "Parity pane",
      checked: true,
    });
    const layout = useViewerStore.getState().rightPaneLayout;
    const reloaded = reconcileLayoutToBuiltins(JSON.parse(JSON.stringify(layout)));
    expect(findPanelLocation(reloaded, PARITY)).not.toBeNull();
  });
});
