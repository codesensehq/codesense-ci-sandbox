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

// Seam: a pane runs behind the REAL `window.codesense` shim and its data reads (both
// lifted from viewer.rs) and talks over postMessage to the REAL right-pane host,
// HtmlViewer, bridge and Tauri command wrappers. Only the Tauri IPC and its events are
// stubbed, standing in for the backend (whose reads are the MCP tools' own functions,
// covered by their Rust tests).
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
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { PaneListing } from "@/tauri/commands";
import {
  MACHINES_CHANGED_EVENT,
  ROUTINES_CHANGED_EVENT,
  WORKTREE_CHANGED_EVENT,
} from "@/tauri/events";
import type { Org } from "@/shared/types/domain";

import { getPanel } from "../panel-registry";
import { defaultRightPaneLayout } from "../right-pane-layout";
import { PanePanelsMount } from "./use-register-pane-panels";

const PORT = 9323;
const PaneMessageEvent = frameEventClass(PORT);
const ORIGIN = `http://f-test.localhost:${PORT}`;
const ORG = "acme/api";
const TEAM = "backend";
const ROOT = "/h/users/42/orgs/acme/api";
const CONTENT = `${ROOT}/${TEAM}/content`;
const PANEL = `pane:${ORG}:${TEAM}/ops`;

const SHIM = shimSource();

// An ops pane over routines, runs, worktrees and machines, re-reading on each nudge.
const PANE = `<!doctype html><html><head><title>Ops</title></head><body>
<p id="routines">…</p><p id="history">…</p><p id="run">…</p><p id="runs">…</p>
<p id="detail">…</p><p id="evidence">…</p><p id="branch">…</p><p id="worktrees">…</p><p id="machines">…</p><p id="nudges">0,0,0</p>
<script>
  var n = { r: 0, m: 0, w: 0 };
  function nudged() { document.getElementById("nudges").textContent = n.r + "," + n.m + "," + n.w; }
  codesense.onRoutinesChanged(function () { n.r++; nudged(); });
  codesense.onMachinesChanged(function () { n.m++; nudged(); });
  codesense.onWorktreesChanged(function () { n.w++; nudged(); });
  codesense.routines().then(function (list) {
    document.getElementById("routines").textContent = list.map(function (r) { return r.name; }).join(",");
    return codesense.routineRuns({ key: list[0].id, limit: 5 });
  }).then(function (page) {
    document.getElementById("history").textContent = page.runs.map(function (r) { return r.phase; }).join(",");
    return codesense.routineRun({ runId: page.runs[0].run_id });
  }).then(function (run) {
    document.getElementById("run").textContent = run.routine_name + ":" + run.phase;
  });
  codesense.runs({ limit: 2 }).then(function (rows) {
    document.getElementById("runs").textContent = rows.map(function (r) { return r.outcome.run_id + "=" + r.validated; }).join(",");
    return codesense.run({ runId: rows[0].outcome.run_id });
  }).then(function (r) {
    document.getElementById("detail").textContent = r.artifact + "|" + r.artifactTruncated;
    return codesense.runEvidence({ runId: r.outcome ? r.outcome.run_id : "x2" });
  }).then(function (ev) {
    document.getElementById("evidence").textContent =
      ev.header.storyId + ":" + ev.run.steps.map(function (st) { return st.nodeId + "=" + st.attempts[0].result; }).join(",") +
      ":" + ev.perNodeResults.map(function (n) { return n.summary; }).join(",") + ":" + ev.authority;
    return codesense.runWorkProduct({ runId: ev.header.runId });
  }).then(function (wp) {
    document.getElementById("branch").textContent = wp ? wp.branch : "none";
  });
  codesense.worktrees().then(function (l) {
    document.getElementById("worktrees").textContent = l.rows.map(function (w) { return w.storyId; }).join(",");
  });
  codesense.machines().then(function (ms) {
    document.getElementById("machines").textContent = ms.map(function (m) { return m.label; }).join(",");
  });
</script></body></html>`;

function listing(): PaneListing {
  return {
    contentRoot: CONTENT,
    chain: [TEAM],
    panes: [
      {
        id: "ops",
        scope: "team",
        team: TEAM,
        title: "Ops",
        path: `${ROOT}/${TEAM}/panes/ops.html`,
        panesDir: `${ROOT}/${TEAM}/panes`,
        panelId: PANEL,
      },
    ],
  };
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
    case "routine_read_list":
      return [{ id: `${ORG}/${TEAM}/digest`, name: "digest", last_run: null }];
    case "routine_read_runs":
      return {
        runs: [
          { run_id: "r2", phase: "needs_you" },
          { run_id: "r1", phase: "succeeded" },
        ],
        next_before: null,
      };
    case "routine_read_run":
      return { run_id: args?.runId, routine_name: "digest", phase: "needs_you" };
    case "run_read_list":
      return [
        { outcome: { run_id: "x2" }, validated: "failed" },
        { outcome: { run_id: "x1" }, validated: "completed" },
      ];
    case "run_read":
      return { outcome: null, transcript: null, artifact: "tail", artifactTruncated: true };
    case "run_read_evidence":
      return {
        header: { runId: args?.runId, storyId: "SW-1", trigger: { kind: "preEnvelope" } },
        run: {
          runId: args?.runId,
          correlationId: args?.runId,
          steps: [
            {
              stepExecutionId: "x2::dev",
              nodeId: "dev",
              attempts: [
                { attemptId: "x2::dev#1", attemptOrdinal: 1, runId: "x2", result: "failed" },
              ],
            },
          ],
        },
        perNodeResults: [{ nodeId: "agent", summary: "tests red" }],
        authority: "durable",
      };
    case "run_read_work_product":
      return { branch: "feat/x2" };
    case "list_managed_worktrees":
      return { rows: [{ kind: "story", storyId: "SW-1" }], reaped: 0 };
    case "list_machines":
      return [{ label: "This Mac" }, { label: `${args?.orgSlug}:${args?.team}` }];
    default:
      return null;
  }
}

interface PaneDoc {
  win: DocWindow;
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
  return { win, doc: win.document as unknown as Document };
}

const mockFetch = vi.fn();
globalThis.fetch = mockFetch;

beforeEach(() => {
  liveHub.dispose();
  listeners.clear();
  tauriInvoke.mockReset();
  tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) =>
    stubTauri(cmd, args),
  );
  mockFetch.mockImplementation(async () => ({ ok: true, status: 200, statusText: "" }));
  useOrgStore.setState({
    orgs: [{ slug: ORG, teams: [{ name: TEAM }] } as unknown as Org],
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

async function openOps(): Promise<PaneDoc> {
  render(
    <TooltipProvider>
      <PanePanelsMount />
      <RightPaneLayout repoPath="" onClose={() => {}} onFileSelect={() => {}} />
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
  return bootPaneDoc(frame(), PANE);
}

const text = (pane: PaneDoc, id: string) => pane.doc.getElementById(id)?.textContent;

describe("pane data reads: routines, runs, worktrees, machines (seam)", () => {
  it("each read reaches its Rust command and hands the page the result unchanged", async () => {
    const pane = await openOps();
    await waitFor(() => expect(text(pane, "run")).toBe("digest:needs_you"));
    expect(text(pane, "routines")).toBe("digest");
    expect(text(pane, "history")).toBe("needs_you,succeeded");
    expect(tauriInvoke).toHaveBeenCalledWith("routine_read_runs", {
      key: `${ORG}/${TEAM}/digest`,
      limit: 5,
      before: null,
      frameId: "test",
      page: `${CONTENT}/ops.html`,
    });
    expect(tauriInvoke).toHaveBeenCalledWith("routine_read_run", {
      runId: "r2",
      frameId: "test",
      page: `${CONTENT}/ops.html`,
    });

    await waitFor(() => expect(text(pane, "detail")).toBe("tail|true"));
    expect(text(pane, "runs")).toBe("x2=failed,x1=completed");
    expect(tauriInvoke).toHaveBeenCalledWith("run_read_list", { limit: 2 });
    expect(tauriInvoke).toHaveBeenCalledWith("run_read", {
      runId: "x2",
      maxArtifactBytes: null,
    });

    // Evidence and work product: the page gets read_run_evidence / read_run_work_product's
    // shapes as-is; storyId is left for the backend to resolve from the run.
    await waitFor(() => expect(text(pane, "branch")).toBe("feat/x2"));
    expect(text(pane, "evidence")).toBe("SW-1:dev=failed:tests red:durable");
    expect(tauriInvoke).toHaveBeenCalledWith("run_read_evidence", { runId: "x2", storyId: null });
    expect(tauriInvoke).toHaveBeenCalledWith("run_read_work_product", { runId: "x2" });

    await waitFor(() => expect(text(pane, "worktrees")).toBe("SW-1"));
    // Machines are read for the pane's own team, resolved by the host from its anchor.
    await waitFor(() => expect(text(pane, "machines")).toBe(`This Mac,${ORG}:${TEAM}`));
    expect(tauriInvoke).toHaveBeenCalledWith("list_machines", {
      orgSlug: ORG,
      team: TEAM,
      freshStats: true,
    });
  });

  it("routine, machine and worktree changes reach the live page as nudges", async () => {
    const pane = await openOps();
    await waitFor(() => expect(text(pane, "machines")).not.toBe("…"));
    const emit = (event: string) => {
      for (const cb of listeners.get(event) ?? []) cb({ payload: null });
    };
    await act(async () => emit(ROUTINES_CHANGED_EVENT));
    await waitFor(() => expect(text(pane, "nudges")).toBe("1,0,0"));
    await act(async () => emit(MACHINES_CHANGED_EVENT));
    await waitFor(() => expect(text(pane, "nudges")).toBe("1,1,0"));
    await act(async () => emit(WORKTREE_CHANGED_EVENT));
    await waitFor(() => expect(text(pane, "nudges")).toBe("1,1,1"));
  });
});
