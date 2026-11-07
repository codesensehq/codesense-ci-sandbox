// SPDX-License-Identifier: MIT
vi.mock("@/features/dashboard/lib/verb-dispatch", async () =>
  (await import("@/test/verb-dispatch-mock")).openVerbDispatchMock(),
);
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));
import { shimSource } from "../../../../../tests/support/page-shim";
import { PaneSourceProjection } from "@/app/mock/pane-source-projection";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { retireAfterFlush } from "@/features/dashboard/lib/frame-lifecycle";
import { frameIpc } from "../../../../../tests/support/fixtures/frame";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Seam: the REAL right-pane host and stack, panel registry, pane registration
// hook, pane body, HtmlViewer and bridge, viewer/org stores and Tauri command
// wrappers. Only the Tauri IPC and its event channel are stubbed, standing in for
// the panes on disk and the pane watcher; an "agent" edits those files the way
// pane_write / a save / a delete does and the backend's events follow.
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
vi.mock("@/features/viewer/components/scratch-note-panel", () => ({
  ScratchNotePanel: () => <div data-testid="scratch-note-panel" />,
}));

import { RightPaneLayout } from "@/features/viewer/components/right-pane-layout";
import { bootPaneDoc } from "./pane-seam-helpers";
import { PanePanelsMount } from "./use-register-pane-panels";
import { paneListingCache, usePaneStore } from "./pane-store";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  OwningScopeProvider,
  queryAllShown,
  queryShown,
  useScopeTier,
  warmScopes,
} from "@/shared/lib/warm-scopes";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { PaneEntry, PaneListing } from "@/tauri/commands";
import type { Org } from "@/shared/types/domain";

import { getPanel } from "../panel-registry";
import { defaultRightPaneLayout, INBOX_PANEL_ID, revealPanel } from "../right-pane-layout";
import { registerBuiltinPanels } from "../builtin-panels";
import { expectFlatAcross } from "@/shared/lib/warm-scopes/__test-utils__/expect-flat-across";

const PORT = 9321;
const ORG = "acme/api";
const TEAM = "backend";
const PARENT = "platform";
const ROOT = "/h/users/42/orgs/acme/api";

/** The panes on disk, `${team}/${id}` → html. */
const disk = new Map<string, string>();
/** A pane file's mtime, `${team}/${id}` → RFC3339. */
const modifiedAt = new Map<string, string>();
let sourceTransport: PaneSourceProjection;

function listing(team: string, org = ORG): PaneListing {
  const chain = team === TEAM ? [TEAM, PARENT] : [team];
  const panes: PaneEntry[] = [];
  for (const owner of [...chain, "-"]) {
    for (const [key, html] of disk) {
      const [dir, id] = key.split("/");
      if (dir !== owner) continue;
      const scope = owner === "-" ? "private" : "team";
      panes.push({
        id,
        scope,
        dashboardRef: { scope, org, ...(scope === "team" ? { team: owner } : {}), id },
        ...(scope === "team" ? { team: owner } : {}),
        title: /<title>(.*?)<\/title>/.exec(html)?.[1] ?? id,
        path: `${ROOT}/${owner}/panes/${id}.html`,
        modified: modifiedAt.get(key) ?? "2026-10-25T00:00:00Z",
        panesDir: `${ROOT}/${owner}/panes`,
        panelId: scope === "private" ? `pane:private:${org}/${id}` : `pane:${org}:${owner}/${id}`,
      });
    }
  }
  return { contentRoot: `${ROOT}/${team}/content`, chain, panes };
}

function stubTauri(cmd: string, args: Record<string, unknown> | undefined): unknown {
  const frameResult = frameIpc(cmd, args, PORT);
  if (frameResult !== undefined) return frameResult;
  switch (cmd) {
    case "get_http_api_port":
      return PORT;
    case "list_panes":
      return sourceTransport.read(args ?? {});
    case "live_hub_subscribe":
      return sourceTransport.snapshot(String(args?.viewKey));
    case "list_builtin_panes":
      return { contentRoot: `${ROOT}/${args?.team}/content`, chain: [], panes: [] };
    case "list_attention_board":
      return { youOwe: [], owedToYou: [], onAgent: [], parked: [], doneUnacked: [] };
    case "wv_list_pending_approvals":
      return [];
    case "dashboard_probe_sessions_alive":
      return [];
    default:
      return null;
  }
}

const mockFetch = vi.fn();
globalThis.fetch = mockFetch;

function paneFrame(): HTMLIFrameElement {
  const frame = screen.getByTestId("pane-body").querySelector("iframe");
  if (!frame) throw new Error("no pane iframe");
  return frame;
}

/** Generic file-pane cases do not place an Inbox; routing cases below use the product default. */
function genericPaneLayout() {
  return { sections: defaultRightPaneLayout().sections.slice(1) };
}

beforeEach(() => {
  liveHub.dispose();
  vi.clearAllMocks();
  disk.clear();
  modifiedAt.clear();
  usePaneStore.setState({
    pendingIntents: {},
    knownEntries: {},
    listing: null,
    teamKey: null,
    listError: null,
  });
  listeners.clear();
  sourceTransport = new PaneSourceProjection(
    (org, team) => listing(team, org),
    (event, payload) => {
      for (const cb of listeners.get(event) ?? []) cb({ payload });
    },
  );
  tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) =>
    stubTauri(cmd, args),
  );
  // HEAD pre-check: a pane on disk is served, a deleted one 404s.
  mockFetch.mockImplementation(async (url: string) => {
    const m = /\/pane\/[^/]+\/[^/]+\/([^?]+)/.exec(url);
    const id = m?.[1].replace(/\.html$/, "");
    const present = [...disk.keys()].some((k) => k.endsWith(`/${id}`));
    return { ok: present, status: present ? 200 : 404, statusText: "" };
  });
  useOrgStore.setState({
    orgs: [{ slug: ORG } as unknown as Org],
    activeOrgSlug: ORG,
    activeTeamName: TEAM,
  });
  useViewerStore.setState({ rightPaneLayout: genericPaneLayout(), rightPaneOpen: false });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

// A switch between teams keeps the right pane steady: a team shown again renders from its cached
// listing in its first frame, a never-listed team shows its own loading state until its listing lands,
// the registry never loses a panel while a listing is in flight, and the pane-source watch never stacks.
const ORG2 = "zed/other";
const NOTES = `pane:private:${ORG}/board`;
const DESIGN_KEY = `${ORG}:design`;
const BACKEND_KEY = `${ORG}:${TEAM}`;
let pendingLists: Record<string, () => void> = {};

function holdList(team: string) {
  let release!: () => void;
  const gate = new Promise<void>((resolve) => (release = resolve));
  pendingLists[team] = release;
  const inner = tauriInvoke.getMockImplementation()!;
  tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "list_panes" && args?.team === team && pendingLists[team] === release) await gate;
    return inner(cmd, args);
  });
  return release;
}

const bodyChildren = () => [...screen.getByTestId("pane-body").children] as HTMLElement[];
const shownPage = () => bodyChildren().filter((c) => c.style.display !== "none");

async function mountPane() {
  disk.set("-/board", "<title>Board</title>");
  render(
    <>
      <PanePanelsMount />
      <RightPaneLayout repoPath="/repo" onClose={() => {}} />
    </>,
  );
  await waitFor(() => expect(getPanel(NOTES)).toBeDefined());
  await act(async () => useViewerStore.getState().revealPanelById(NOTES));
  await waitFor(() => expect(paneFrame().getAttribute("src")).toContain("/pane/"));
}

describe("a team switch keeps the right pane steady", () => {
  beforeEach(() => {
    pendingLists = {};
    warmScopes.forget(DESIGN_KEY);
    warmScopes.forget(BACKEND_KEY);
    warmScopes.activate(BACKEND_KEY);
    useOrgStore.setState({
      orgs: [
        {
          slug: ORG,
          teams: [
            { name: TEAM, source_repos: [] },
            { name: "design", source_repos: [] },
          ],
        } as unknown as Org,
      ],
    });
    useViewerStore.setState({ rightPaneOpen: true });
  });

  it("a team shown again renders its cached listing in its first frame, with no Loading and no fallback strip", async () => {
    await mountPane();
    await act(async () => useOrgStore.getState().setActiveTeam("design"));
    await waitFor(() => expect(usePaneStore.getState().teamKey).toBe(DESIGN_KEY));
    // Back to a team whose listing is cached while its re-read is still in flight.
    holdList(TEAM);
    await act(async () => useOrgStore.getState().setActiveTeam(TEAM));
    expect(screen.queryByText("Loading…")).toBeNull();
    expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
    expect(shownPage()).toHaveLength(1);
    expect(shownPage()[0].querySelector("iframe")).not.toBeNull();
    expect(usePaneStore.getState().teamKey).toBe(BACKEND_KEY);
    pendingLists[TEAM]();
  });

  it("a pane's frame stays transparent over the themed surface until its page reports ready", async () => {
    await mountPane();
    const frame = paneFrame();
    expect(frame.style.opacity).toBe("0");
    // Invisible is also unreachable: no Tab or Enter lands in a document the user cannot see.
    expect(frame.inert).toBe(true);
    expect(frame.style.background).not.toBe("");
    expect(frame.style.colorScheme).not.toBe("");
    actEnv.IS_REACT_ACT_ENVIRONMENT = false;
    bootNewFrames();
    await waitFor(() => expect(frame.style.opacity).toBe(""));
    expect(frame.inert).toBe(false);
    actEnv.IS_REACT_ACT_ENVIRONMENT = true;
  });

  describe("a page that has not yet applied its theme", () => {
    // The shim applies the theme and then acks it; this one applies it and stays silent.
    const silentShim = shimSource().replace("'codesense:theme-applied'", "'codesense:silent'");

    it("stays transparent after shim-ready until the theme ack, then reveals", async () => {
      await mountPane();
      const frame = paneFrame();
      actEnv.IS_REACT_ACT_ENVIRONMENT = false;
      const [pane] = bootNewFrames(silentShim);
      await waitFor(() =>
        expect(pane.doc.documentElement.style.getPropertyValue("--cs-color-scheme")).not.toBe(""),
      );
      await new Promise((r) => setTimeout(r, 50));
      expect(frame.style.opacity).toBe("0");
      expect(frame.inert).toBe(true);
      pane.win.parent.postMessage({ token: "frame-token", type: "codesense:theme-applied" }, "*");
      await waitFor(() => expect(frame.style.opacity).toBe(""));
      expect(frame.inert).toBe(false);
      actEnv.IS_REACT_ACT_ENVIRONMENT = true;
    });

    it("reveals on the fallback when the ack never arrives", async () => {
      await mountPane();
      const frame = paneFrame();
      actEnv.IS_REACT_ACT_ENVIRONMENT = false;
      const [pane] = bootNewFrames(silentShim);
      await waitFor(() =>
        expect(pane.doc.documentElement.style.getPropertyValue("--cs-color-scheme")).not.toBe(""),
      );
      expect(frame.style.opacity).toBe("0");
      await waitFor(() => expect(frame.style.opacity).toBe(""), { timeout: 2000 });
      actEnv.IS_REACT_ACT_ENVIRONMENT = true;
    });
  });

  it("a retired frame stays hidden and inert when its page then reports ready", async () => {
    await mountPane();
    const frame = paneFrame();
    await act(async () => {
      void retireAfterFlush(
        (f) => f.element === frame,
        () => {},
        50,
      );
    });
    expect(frame.style.visibility).toBe("hidden");
    actEnv.IS_REACT_ACT_ENVIRONMENT = false;
    bootNewFrames();
    await waitFor(() => expect(frame.style.opacity).toBe(""));
    actEnv.IS_REACT_ACT_ENVIRONMENT = true;
    expect(frame.inert).toBe(true);
    expect(frame.style.visibility).toBe("hidden");
  });

  it("a team never listed shows its own loading state until its listing lands", async () => {
    await mountPane();
    holdList("design");
    await act(async () => useOrgStore.getState().setActiveTeam("design"));
    // Its own state, as on a first visit: the previous team's page is not held on screen.
    expect(shownPage()).toHaveLength(0);
    expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
    await act(async () => pendingLists["design"]());
    await waitFor(() => expect(usePaneStore.getState().teamKey).toBe(DESIGN_KEY));
    await waitFor(() => expect(shownPage()).toHaveLength(1));
    expect(shownPage()[0].hasAttribute("inert")).toBe(false);
  });

  it("a cross-org return registers the returning team's cached panes before its listing lands", async () => {
    const IDEAS = `pane:${ORG}:${TEAM}/ideas`;
    disk.set("backend/ideas", "<title>Ideas</title>");
    useOrgStore.setState((s) => ({
      orgs: [
        ...s.orgs,
        { slug: ORG2, teams: [{ name: "ops", source_repos: [] }] } as unknown as Org,
      ],
    }));
    await mountPane();
    await waitFor(() => expect(getPanel(IDEAS)).toBeDefined());
    await act(async () => {
      useOrgStore.getState().setActiveOrg(ORG2);
      useOrgStore.getState().setActiveTeam("ops");
    });
    await waitFor(() => expect(usePaneStore.getState().teamKey).toBe(`${ORG2}:ops`));
    await waitFor(() => expect(getPanel(IDEAS)).toBeUndefined());
    holdList(TEAM);
    await act(async () => {
      useOrgStore.getState().setActiveOrg(ORG);
      useOrgStore.getState().setActiveTeam(TEAM);
    });
    expect(getPanel(IDEAS)).toBeDefined();
    pendingLists[TEAM]();
  });

  it("a renamed team does not adopt the old team's listing, with its paths and identities, as its own", async () => {
    const RENAMED_KEY = `${ORG}:renamed`;
    disk.set("backend/ideas", "<title>Ideas</title>");
    await mountPane();
    await waitFor(() => expect(getPanel(`pane:${ORG}:${TEAM}/ideas`)).toBeDefined());
    holdList("renamed");
    await act(async () => {
      warmScopes.rekey(BACKEND_KEY, RENAMED_KEY);
      useOrgStore.setState({ activeTeamName: "renamed" });
    });
    const state = usePaneStore.getState();
    expect(paneListingCache.get(RENAMED_KEY)).toBeUndefined();
    expect(state.teamKey === RENAMED_KEY && state.listing?.contentRoot.includes("/backend/")).toBe(
      false,
    );
    await act(async () => pendingLists["renamed"]());
    warmScopes.forget(RENAMED_KEY);
  });

  it("no panel leaves the registry while the incoming listing is in flight", async () => {
    await mountPane();
    holdList("design");
    const seen: boolean[] = [];
    const stop = (await import("../panel-registry")).usePanelRegistry.subscribe((s) =>
      seen.push(s.panels[NOTES] !== undefined),
    );
    await act(async () => useOrgStore.getState().setActiveTeam("design"));
    expect(getPanel(NOTES)).toBeDefined();
    await act(async () => pendingLists["design"]());
    await waitFor(() => expect(usePaneStore.getState().teamKey).toBe(DESIGN_KEY));
    stop();
    expect(seen.every(Boolean)).toBe(true);
  });
});

// Switches run outside `act`, so React commits on its own schedule and the observers see each
// committed state, as a user's screen would, instead of one batch per act scope.
const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const settle = async (key: string) => {
  await waitFor(() => expect(usePaneStore.getState().teamKey).toBe(key));
  for (let i = 0; i < 3; i++) await tick();
};
const actEnv = globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean };

/** A frame the host mounts boots its page, as the real shim does, so the host goes live. */
const booted = new WeakSet<Element>();
function bootNewFrames(shim?: string) {
  const docs: ReturnType<typeof bootPaneDoc>[] = [];
  for (const frame of document.querySelectorAll("iframe")) {
    if (booted.has(frame) || !frame.src.includes("/pane/")) continue;
    booted.add(frame);
    docs.push(
      bootPaneDoc(
        frame,
        "<script>codesense.ready();</script>",
        { origin: `http://f-test.localhost:${PORT}`, token: "frame-token" },
        false,
        { shim },
      ),
    );
  }
  return docs;
}

// 50 round trips of what a user can see in the right pane, after a two-round warm-up. Every counter
// holds flat: no Loading frame, no blank pane, no fallback strip, and no held watch left behind.
const NOTES2 = `pane:private:${ORG2}/board`;
const OTHER_KEY = `${ORG2}:ops`;
const FIFTY_ROUND_TRIPS_MS = 60_000;

describe("flat across 50 round trips", () => {
  const seen = { loading: 0, blank: 0, strip: 0 };
  let stopWatching = () => {};
  const read = () => ({ ...seen, holds: warmScopes.debugCounts().holdsTotal });
  /** What the column shows after each commit: a Loading frame, no page at all, or the amber strip. */
  function watchColumn() {
    const observer = new MutationObserver(() => {
      bootNewFrames();
      const body = document.querySelector<HTMLElement>("[data-testid='pane-body']");
      const loading = queryAllShown("div").some((el) => el.textContent === "Loading…");
      if (loading) seen.loading += 1;
      const pages = body
        ? [...body.children].filter((c) => (c as HTMLElement).style.display !== "none")
        : [];
      if (pages.length === 0) seen.blank += 1;
      if (queryShown("[data-testid='slot-fallback-strip']")) seen.strip += 1;
    });
    observer.observe(document.body, { subtree: true, childList: true, attributes: true });
    stopWatching = () => observer.disconnect();
  }

  beforeEach(() => {
    Object.assign(seen, { loading: 0, blank: 0, strip: 0 });
    for (const key of [BACKEND_KEY, DESIGN_KEY, OTHER_KEY]) warmScopes.forget(key);
    warmScopes.activate(BACKEND_KEY);
    useOrgStore.setState({
      orgs: [
        {
          slug: ORG,
          teams: [
            { name: TEAM, source_repos: [] },
            { name: "design", source_repos: [] },
          ],
        },
        { slug: ORG2, teams: [{ name: "ops", source_repos: [] }] },
      ] as unknown as Org[],
    });
    useViewerStore.setState({
      rightPaneOpen: true,
      rightPaneLayout: revealPanel(revealPanel(genericPaneLayout(), NOTES2), NOTES),
    });
  });
  afterEach(() => {
    stopWatching();
    actEnv.IS_REACT_ACT_ENVIRONMENT = true;
  });

  async function mountBoth() {
    disk.set("-/board", "<title>Board</title>");
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );
    await waitFor(() => expect(getPanel(NOTES)).toBeDefined());
    await act(async () => useViewerStore.getState().revealPanelById(NOTES));
    await waitFor(() => expect(paneFrame().getAttribute("src")).toContain("/pane/"));
    actEnv.IS_REACT_ACT_ENVIRONMENT = false;
    watchColumn();
  }

  it(
    "A<->B in one org",
    async () => {
      await mountBoth();
      await expectFlatAcross(async () => {
        useOrgStore.getState().setActiveTeam("design");
        await settle(DESIGN_KEY);
        useOrgStore.getState().setActiveTeam(TEAM);
        await settle(BACKEND_KEY);
      }, read);
    },
    FIFTY_ROUND_TRIPS_MS,
  );

  it(
    "A<->C across orgs",
    async () => {
      await mountBoth();
      await expectFlatAcross(async () => {
        useOrgStore.getState().setActiveOrg(ORG2);
        useOrgStore.getState().setActiveTeam("ops");
        await settle(OTHER_KEY);
        useOrgStore.getState().setActiveOrg(ORG);
        useOrgStore.getState().setActiveTeam(TEAM);
        await settle(BACKEND_KEY);
      }, read);
    },
    FIFTY_ROUND_TRIPS_MS,
  );
});

// The workbench mounts one right-pane layout per opened team, each in its own host and each under
// its team's owning scope. A warm team keeps its page frames, hidden: switching never re-mints.
describe("every team's own right pane across 50 round trips", () => {
  const mints = () => tauriInvoke.mock.calls.filter(([cmd]) => cmd === "mint_frame").length;
  function TeamHost({ teamKey }: { teamKey: string }) {
    const tier = useScopeTier(teamKey);
    return (
      <div
        data-right-pane-host={teamKey}
        style={{ display: tier === "active" ? undefined : "none" }}
      >
        <OwningScopeProvider scope={teamKey}>
          <RightPaneLayout repoPath="/repo" onClose={() => {}} />
        </OwningScopeProvider>
      </div>
    );
  }

  const hostOf = (key: string) =>
    document.querySelector<HTMLElement>(`[data-right-pane-host="${key}"]`)!;
  const iframesIn = (key: string) => hostOf(key).querySelectorAll("iframe").length;

  beforeEach(() => {
    for (const key of [BACKEND_KEY, DESIGN_KEY, OTHER_KEY]) warmScopes.forget(key);
    warmScopes.activate(BACKEND_KEY);
    useOrgStore.setState({
      orgs: [
        {
          slug: ORG,
          teams: [
            { name: TEAM, source_repos: [] },
            { name: "design", source_repos: [] },
          ],
        },
      ] as unknown as Org[],
    });
    useViewerStore.setState({
      rightPaneOpen: true,
      rightPaneLayout: revealPanel(genericPaneLayout(), NOTES),
    });
  });
  afterEach(() => {
    actEnv.IS_REACT_ACT_ENVIRONMENT = true;
  });

  it(
    "A<->B mints no right-pane frame, common or team-owned, once both teams have shown theirs",
    async () => {
      const A_OWN = `pane:${ORG}:${TEAM}/ideas`;
      const B_OWN = `pane:${ORG}:design/plans`;
      disk.set("-/board", "<title>Board</title>");
      disk.set("backend/ideas", "<title>Ideas</title>");
      disk.set("design/plans", "<title>Plans</title>");
      const stacked = revealPanel(
        revealPanel(revealPanel(genericPaneLayout(), NOTES), A_OWN),
        B_OWN,
      );
      useViewerStore.setState({
        rightPaneLayout: {
          sections: stacked.sections.map((section) => ({ ...section, collapsed: false })),
        },
      });
      render(
        <>
          <PanePanelsMount />
          <TeamHost teamKey={BACKEND_KEY} />
          <TeamHost teamKey={DESIGN_KEY} />
        </>,
      );
      await waitFor(() => expect(getPanel(A_OWN)).toBeDefined());
      actEnv.IS_REACT_ACT_ENVIRONMENT = false;
      const observer = new MutationObserver(() => bootNewFrames());
      observer.observe(document.body, { subtree: true, childList: true });
      bootNewFrames();
      // Each team shows its own panes once before the count starts.
      useOrgStore.getState().setActiveTeam("design");
      await settle(DESIGN_KEY);
      useOrgStore.getState().setActiveTeam(TEAM);
      await settle(BACKEND_KEY);
      // Backend's host: the private pane and its own; design's: the private pane and its own.
      await waitFor(() => expect(iframesIn(BACKEND_KEY)).toBe(2));
      await waitFor(() => expect(iframesIn(DESIGN_KEY)).toBe(2));
      await expectFlatAcross(
        async () => {
          useOrgStore.getState().setActiveTeam("design");
          await settle(DESIGN_KEY);
          useOrgStore.getState().setActiveTeam(TEAM);
          await settle(BACKEND_KEY);
        },
        () => ({
          rightPaneMints: mints(),
          backendFrames: iframesIn(BACKEND_KEY),
          designFrames: iframesIn(DESIGN_KEY),
        }),
      );
      observer.disconnect();
    },
    FIFTY_ROUND_TRIPS_MS,
  );

  it("a team that owns inbox.html and one that does not each keep their Inbox frame across A->B->A", async () => {
    disk.set("backend/inbox", "<title>Backend Inbox</title>");
    disk.set("-/inbox", "<title>Inbox</title>");
    registerBuiltinPanels();
    useViewerStore.setState({
      rightPaneLayout: {
        sections: defaultRightPaneLayout().sections.map((section) => ({
          ...section,
          collapsed: false,
        })),
      },
    });
    render(
      <>
        <PanePanelsMount />
        <TeamHost teamKey={BACKEND_KEY} />
        <TeamHost teamKey={DESIGN_KEY} />
      </>,
    );
    await waitFor(() => expect(getPanel(INBOX_PANEL_ID)).toBeDefined());
    actEnv.IS_REACT_ACT_ENVIRONMENT = false;
    const observer = new MutationObserver(() => bootNewFrames());
    observer.observe(document.body, { subtree: true, childList: true });
    bootNewFrames();
    const frameOf = (key: string) =>
      hostOf(key).querySelector<HTMLIFrameElement>("[data-testid='pane-body'] iframe");
    useOrgStore.getState().setActiveTeam("design");
    await settle(DESIGN_KEY);
    useOrgStore.getState().setActiveTeam(TEAM);
    await settle(BACKEND_KEY);
    await waitFor(() => expect(frameOf(BACKEND_KEY)).not.toBeNull());
    await waitFor(() => expect(frameOf(DESIGN_KEY)).not.toBeNull());
    const before = { a: frameOf(BACKEND_KEY)!, b: frameOf(DESIGN_KEY)! };
    const minted = mints();
    expect(before.a.getAttribute("src")).not.toBe(before.b.getAttribute("src"));
    useOrgStore.getState().setActiveTeam("design");
    await settle(DESIGN_KEY);
    useOrgStore.getState().setActiveTeam(TEAM);
    await settle(BACKEND_KEY);
    expect(frameOf(BACKEND_KEY)).toBe(before.a);
    expect(frameOf(DESIGN_KEY)).toBe(before.b);
    expect(mints()).toBe(minted);
    observer.disconnect();
  });

  // The source-refresh path mints its replacement after a list read, a resync and a 150 ms timer,
  // so a counter read right after the switch settles would miss it.
  const REFRESH_WINDOW_MS = 600;

  it("fresh boot, then A->B->A: a warm return mints no replacement for the shared Inbox", async () => {
    disk.set("-/inbox", "<title>Inbox</title>");
    registerBuiltinPanels();
    useViewerStore.setState({
      rightPaneLayout: {
        sections: defaultRightPaneLayout().sections.map((section) => ({
          ...section,
          collapsed: false,
        })),
      },
    });
    render(
      <>
        <PanePanelsMount />
        <TeamHost teamKey={BACKEND_KEY} />
        <TeamHost teamKey={DESIGN_KEY} />
      </>,
    );
    await waitFor(() => expect(getPanel(INBOX_PANEL_ID)).toBeDefined());
    actEnv.IS_REACT_ACT_ENVIRONMENT = false;
    const observer = new MutationObserver(() => bootNewFrames());
    observer.observe(document.body, { subtree: true, childList: true });
    bootNewFrames();
    const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
    const hop = async (team: string, key: string) => {
      useOrgStore.getState().setActiveTeam(team);
      await settle(key);
      await wait(REFRESH_WINDOW_MS);
    };
    await hop("design", DESIGN_KEY);
    await hop(TEAM, BACKEND_KEY);
    await waitFor(() => expect(iframesIn(BACKEND_KEY)).toBeGreaterThan(0));
    await waitFor(() => expect(iframesIn(DESIGN_KEY)).toBeGreaterThan(0));
    const minted = mints();
    await hop("design", DESIGN_KEY);
    await hop(TEAM, BACKEND_KEY);
    await hop("design", DESIGN_KEY);
    await hop(TEAM, BACKEND_KEY);
    expect(mints()).toBe(minted);
    observer.disconnect();
  });

  describe("a team-owned Inbox, fresh boot, then a cross-org warm return", () => {
    // Rust remembers a team-owned document whose last source was pruned (`pane_sources.rs`
    // `PrunedDocument`), so an unchanged return keeps its `documentRevision` and the page stays.
    async function bootBackendThenOps() {
      disk.set("backend/inbox", "<title>Backend Inbox</title>");
      disk.set("-/inbox", "<title>Inbox</title>");
      registerBuiltinPanels();
      useOrgStore.setState({
        orgs: [
          { slug: ORG, teams: [{ name: TEAM, source_repos: [] }] },
          { slug: ORG2, teams: [{ name: "ops", source_repos: [] }] },
        ] as unknown as Org[],
      });
      useViewerStore.setState({
        rightPaneLayout: {
          sections: defaultRightPaneLayout().sections.map((section) => ({
            ...section,
            collapsed: false,
          })),
        },
      });
      render(
        <>
          <PanePanelsMount />
          <TeamHost teamKey={BACKEND_KEY} />
          <TeamHost teamKey={OTHER_KEY} />
        </>,
      );
      await waitFor(() => expect(getPanel(INBOX_PANEL_ID)).toBeDefined());
      actEnv.IS_REACT_ACT_ENVIRONMENT = false;
      const observer = new MutationObserver(() => bootNewFrames());
      observer.observe(document.body, { subtree: true, childList: true });
      bootNewFrames();
      const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
      const hop = async (org: string, team: string, key: string) => {
        useOrgStore.getState().setActiveOrg(org);
        useOrgStore.getState().setActiveTeam(team);
        await settle(key);
        await wait(REFRESH_WINDOW_MS);
      };
      await hop(ORG2, "ops", OTHER_KEY);
      await hop(ORG, TEAM, BACKEND_KEY);
      await waitFor(() => expect(iframesIn(BACKEND_KEY)).toBeGreaterThan(0));
      await waitFor(() => expect(iframesIn(OTHER_KEY)).toBeGreaterThan(0));
      return { observer, hop };
    }

    it("mints no replacement", async () => {
      const { observer, hop } = await bootBackendThenOps();
      const minted = mints();
      await hop(ORG2, "ops", OTHER_KEY);
      await hop(ORG, TEAM, BACKEND_KEY);
      expect(mints()).toBe(minted);
      observer.disconnect();
    });

    it("still reloads on return when the HTML was edited while the team was away", async () => {
      const { observer, hop } = await bootBackendThenOps();
      await hop(ORG2, "ops", OTHER_KEY);
      const minted = mints();
      disk.set("backend/inbox", "<title>Backend Inbox</title><p>edited</p>");
      modifiedAt.set("backend/inbox", "2026-10-25T01:00:00Z");
      await hop(ORG, TEAM, BACKEND_KEY);
      await waitFor(() => expect(mints()).toBeGreaterThan(minted));
      observer.disconnect();
    });

    it("reloads on return when only an asset changed while the team was away", async () => {
      const { observer, hop } = await bootBackendThenOps();
      await hop(ORG2, "ops", OTHER_KEY);
      const minted = mints();
      await sourceTransport.changed({ org: ORG, scope: "team", team: TEAM });
      await hop(ORG, TEAM, BACKEND_KEY);
      await waitFor(() => expect(mints()).toBeGreaterThan(minted));
      observer.disconnect();
    });
  });

  it("a retained page's badge is shown again when its team returns", async () => {
    disk.set("-/board", "<title>Board</title>");
    render(
      <>
        <PanePanelsMount />
        <TeamHost teamKey={BACKEND_KEY} />
        <TeamHost teamKey={DESIGN_KEY} />
      </>,
    );
    await waitFor(() => expect(getPanel(NOTES)).toBeDefined());
    actEnv.IS_REACT_ACT_ENVIRONMENT = false;
    const frameA = await waitFor(() => {
      const el = hostOf(BACKEND_KEY).querySelector<HTMLIFrameElement>("iframe");
      expect(el?.getAttribute("src")).toContain("/pane/");
      return el!;
    });
    booted.add(frameA);
    const page = bootPaneDoc(frameA, "<script>codesense.ready();</script>", {
      origin: `http://f-test.localhost:${PORT}`,
      token: "frame-token",
    });
    const observer = new MutationObserver(() => bootNewFrames());
    observer.observe(document.body, { subtree: true, childList: true });
    bootNewFrames();
    await page.win.codesense.setBadge({ count: 7, severity: "info" });
    await waitFor(() => expect(usePaneStore.getState().badges[NOTES]?.count).toBe(7));
    useOrgStore.getState().setActiveTeam("design");
    await settle(DESIGN_KEY);
    useOrgStore.getState().setActiveTeam(TEAM);
    await settle(BACKEND_KEY);
    expect(frameA.isConnected).toBe(true);
    await waitFor(() => expect(usePaneStore.getState().badges[NOTES]?.count).toBe(7));
    observer.disconnect();
    await page.win.happyDOM.close();
  });

  // An inherited pane's grant names its source team (`platform`), but the team whose view shows it
  // owns its visibility and its commands.
  describe("an inherited pane open in two teams", () => {
    const INHERITED = `pane:${ORG}:${PARENT}/board`;
    const COMMANDS = "<script>window.commands=[];codesense.onCommand(c=>window.commands.push(c));";
    async function mountInherited() {
      const { frameFixture } = await import("../../../../../tests/support/fixtures/frame");
      const base = tauriInvoke.getMockImplementation()!;
      const records = new Map<string, ReturnType<typeof frameFixture>>();
      let seq = 0;
      tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
        const a = args as { args: Parameters<typeof frameFixture>[0]; origin?: string };
        if (cmd === "mint_frame") {
          const r = frameFixture(a.args, PORT);
          r.grant.team = { org: ORG, team: PARENT };
          r.frameId = `inh-${++seq}`;
          r.origin = `http://f-${r.frameId}.localhost:${PORT}`;
          r.token = r.frameId;
          records.set(r.origin, r);
          return r;
        }
        if (cmd === "resolve_frame_grant") return records.get(a.origin ?? "")?.grant;
        return base(cmd, args);
      });
      disk.set("platform/board", "<title>Board</title>");
      sourceTransport = new PaneSourceProjection(
        (org, team) => ({
          ...listing(TEAM, org),
          contentRoot: `${ROOT}/${team}/content`,
          chain: [team, PARENT],
        }),
        (event, payload) => {
          for (const cb of listeners.get(event) ?? []) cb({ payload });
        },
      );
      useViewerStore.setState({ rightPaneLayout: revealPanel(genericPaneLayout(), INHERITED) });
      render(
        <>
          <PanePanelsMount />
          <TeamHost teamKey={BACKEND_KEY} />
          <TeamHost teamKey={DESIGN_KEY} />
        </>,
      );
      const frameIn = (key: string) => hostOf(key).querySelector<HTMLIFrameElement>("iframe");
      await waitFor(() => expect(frameIn(BACKEND_KEY)).not.toBeNull());
      const boot = (key: string, body: string) => {
        const frame = frameIn(key)!;
        return bootPaneDoc(frame, body, records.get(new URL(frame.src).origin)!);
      };
      return {
        frameIn,
        boot,
        record: (key: string) => records.get(new URL(frameIn(key)!.src).origin)!,
      };
    }
    const wait = (ms: number) =>
      act(async () => void (await new Promise((r) => setTimeout(r, ms))));
    const commandsOf = (page: ReturnType<typeof bootPaneDoc>) =>
      page.win.eval("window.commands") as { id: string }[];

    it("a hidden team's late page keeps the command queued for the team on screen", async () => {
      const { postDashboardCommand } = await import("@/features/dashboard/lib/frame-lifecycle");
      const { frameIn, boot, record } = await mountInherited();
      const a = boot(BACKEND_KEY, COMMANDS + "</script>");
      await wait(50);
      await act(async () => useOrgStore.getState().setActiveTeam("design"));
      await settle(DESIGN_KEY);
      await waitFor(() => expect(frameIn(DESIGN_KEY)).not.toBeNull());
      expect(warmScopes.tierOf(BACKEND_KEY)).toBe("warm");
      await wait(400);
      postDashboardCommand(record(BACKEND_KEY).grant.dashboard, "right-pane", {
        panelId: INHERITED,
        id: "find",
        label: "Find",
        dashboardTitle: "Board",
      });
      await act(async () => {
        await a.win.codesense.ready();
      });
      await wait(50);
      expect(commandsOf(a)).toEqual([]);
      await a.win.happyDOM.close();
    });

    it("an immediate command reaches the team on screen, not the hidden one", async () => {
      const { postDashboardCommand } = await import("@/features/dashboard/lib/frame-lifecycle");
      const { frameIn, boot, record } = await mountInherited();
      const a = boot(BACKEND_KEY, COMMANDS + "codesense.ready();</script>");
      await wait(50);
      await act(async () => useOrgStore.getState().setActiveTeam("design"));
      await settle(DESIGN_KEY);
      await waitFor(() => expect(frameIn(DESIGN_KEY)).not.toBeNull());
      await wait(400);
      const b = boot(DESIGN_KEY, COMMANDS + "codesense.ready();</script>");
      await wait(100);
      postDashboardCommand(record(BACKEND_KEY).grant.dashboard, "right-pane", {
        panelId: INHERITED,
        id: "immediate-find",
        label: "Find",
        dashboardTitle: "Board",
      });
      await wait(50);
      expect(commandsOf(a).filter((c) => c.id === "immediate-find")).toEqual([]);
      expect(commandsOf(b).filter((c) => c.id === "immediate-find")).toHaveLength(1);
      await a.win.happyDOM.close();
      await b.win.happyDOM.close();
    });
  });
});
