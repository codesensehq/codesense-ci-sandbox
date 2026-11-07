// SPDX-License-Identifier: MIT
vi.mock("@/features/dashboard/lib/verb-dispatch", async () =>
  (await import("@/test/verb-dispatch-mock")).openVerbDispatchMock(),
);
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));
import { PaneSourceProjection } from "@/app/mock/pane-source-projection";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import {
  frameIpc,
  frameEventClass,
  frameFixture,
} from "../../../../../tests/support/fixtures/frame";
import { fireEvent, act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

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
import { PanePanelsMount } from "./use-register-pane-panels";
import { bootPaneDoc } from "./pane-seam-helpers";
import { usePaneStore } from "./pane-store";
import {
  setSurfaceDashboard,
  useDashboardProfileStore,
} from "@/features/dashboard/lib/dashboard-profile";
import { resolveSlot, revealSlot } from "../../slots/reveal-slot";
import { TooltipProvider } from "@/components/ui/tooltip";
import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { PaneEntry, PaneListing } from "@/tauri/commands";
import type { Org } from "@/shared/types/domain";

import { buildPanelMenuEntries, handlePanelMenuAction } from "../panel-menu";
import { getPanel } from "../panel-registry";
import { NETWORK_PROBLEM_GRACE_MS } from "@/features/dashboard/components/html-viewer";
import {
  defaultRightPaneLayout,
  INBOX_PANEL_ID,
  migrateLegacyRightPaneState,
  findPanelLocation,
  reconcileLayoutToBuiltins,
  revealPanel,
} from "../right-pane-layout";

const PORT = 9321;
const PaneMessageEvent = frameEventClass(PORT);
const ORG = "acme/api";
const TEAM = "backend";
const PARENT = "platform";
const ROOT = "/h/users/42/orgs/acme/api";
const BOARD = `pane:${ORG}:${TEAM}/board`;

/** The panes on disk, `${team}/${id}` → html. */
const disk = new Map<string, string>();
let sourceTransport: PaneSourceProjection;

function listing(team: string): PaneListing {
  const chain = team === TEAM ? [TEAM, PARENT] : [team];
  const panes: PaneEntry[] = [];
  for (const t of [...chain, "-"]) {
    for (const [key, html] of disk) {
      const [owner, id] = key.split("/");
      if (owner !== t) continue;
      const title = /<title>(.*?)<\/title>/.exec(html)?.[1] ?? id;
      panes.push({
        id,
        scope: t === "-" ? "private" : "team",
        dashboardRef: {
          scope: t === "-" ? "private" : "team",
          org: ORG,
          ...(t === "-" ? {} : { team: t }),
          id,
        },
        ...(t === "-" ? {} : { team: t }),
        ...(t === "-" && html.includes('content="human-inbox"')
          ? { surface: "human-inbox" as const }
          : {}),
        title,
        path: `${ROOT}/${t}/panes/${id}.html`,
        panesDir: `${ROOT}/${t}/panes`,
        panelId: t === "-" ? `pane:private:${ORG}/${id}` : `pane:${ORG}:${t}/${id}`,
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

/** The backend's events, as the pane watcher and pane_open emit them. */
async function emit(event: string, payload: unknown) {
  await act(async () => {
    if (event === "pane-changed")
      await sourceTransport.changed(payload as import("@/tauri/events").PaneChangedPayload);
    for (const cb of listeners.get(event) ?? []) cb({ payload });
  });
}

/** What an agent does: write a pane file (pane_write) and let the watcher fire. */
async function agentWrites(team: string, id: string, html: string) {
  disk.set(`${team}/${id}`, html);
  await emit("pane-changed", { scope: "team", org: ORG, team, id });
}

async function agentDeletes(team: string, id: string) {
  disk.delete(`${team}/${id}`);
  await emit("pane-changed", { scope: "team", org: ORG, team, id });
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
  // No team listing cached by an earlier test.
  for (const scope of Object.keys(warmScopes.debugCounts().tiers)) warmScopes.forget(scope);
  liveHub.dispose();
  vi.clearAllMocks();
  disk.clear();
  usePaneStore.setState({
    pendingIntents: {},
    knownEntries: {},
    listing: null,
    teamKey: null,
    listError: null,
  });
  listeners.clear();
  sourceTransport = new PaneSourceProjection(
    (_org, team) => listing(team),
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

describe("a pane is an HTML file (seam)", () => {
  it("leaves shared discovery pending beyond the profile wait bound, then adopts its listing", async () => {
    let land!: (value: PaneListing) => void;
    const pending = new Promise<PaneListing>((resolve) => {
      land = resolve;
    });
    tauriInvoke.mockImplementation(async (cmd, args) =>
      cmd === "list_panes" ? pending : stubTauri(cmd, args),
    );
    usePaneStore.setState({ teamKey: null, listing: null, listError: null });
    vi.useFakeTimers();
    render(<PanePanelsMount />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(4000);
    });
    expect(usePaneStore.getState().listing).toBeNull();
    expect(usePaneStore.getState().listError).toBeNull();
    const result = { contentRoot: "/slow/context", chain: [TEAM], panes: [] };
    await act(async () => {
      land(result);
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(usePaneStore.getState().listing).toEqual(result);
    expect(usePaneStore.getState().listError).toBeNull();
  });

  it.each(["ready", "reject", "hang"])(
    "opens the installed copy and publishes the choice when refresh is %s",
    async (mode) => {
      const sourceRef = { scope: "team" as const, org: ORG, team: TEAM, id: "notes" };
      const copyRef = {
        scope: "private" as const,
        org: ORG,
        id: "installed-notes",
        installedFrom: sourceRef,
      };
      const source: PaneEntry = {
        id: "notes",
        title: "Notes",
        scope: "team",
        team: TEAM,
        surface: "notes",
        dashboardRef: sourceRef,
        panelId: `pane:${ORG}:${TEAM}/notes`,
        path: `${ROOT}/${TEAM}/panes/notes.html`,
        panesDir: `${ROOT}/${TEAM}/panes`,
      };
      const copy = {
        ...source,
        id: copyRef.id,
        scope: "private" as const,
        team: undefined,
        dashboardRef: copyRef,
        panelId: `pane:private:${ORG}/${copyRef.id}`,
        path: `${ROOT}/-/panes/${copyRef.id}.html`,
        panesDir: `${ROOT}/-/panes`,
      };
      let installed = false;
      let recovered = false;
      disk.set(`${TEAM}/notes`, "<title>Notes</title>");
      usePaneStore.setState({ teamKey: null, listing: null, knownEntries: {} });
      useDashboardProfileStore.setState({ org: ORG, profile: null });
      useViewerStore.setState({ rightPaneOpen: true });
      sourceTransport = new PaneSourceProjection(
        () => ({
          contentRoot: `${ROOT}/${TEAM}/content`,
          chain: [TEAM],
          panes: installed ? [source, copy] : [source],
        }),
        (event, payload) => {
          for (const cb of listeners.get(event) ?? []) cb({ payload });
        },
      );
      tauriInvoke.mockImplementation(async (cmd, args) => {
        if (cmd === "list_panes") {
          if (installed && !recovered && mode === "reject") throw new Error("Cannot refresh");
          if (installed && !recovered && mode === "hang") return new Promise(() => {});
          return sourceTransport.read(args ?? {});
        }
        if (cmd === "write_dashboard_profile") {
          installed = true;
          disk.set(`-/${copyRef.id}`, "<title>Notes copy</title>");
          const saved = {
            ...useDashboardProfileStore.getState().profile!,
            rightPaneSurfaces: { notes: copyRef },
          };
          // This host-only fixture receives the app root bridge's committed profile.
          useDashboardProfileStore.setState({ profile: saved });
          return saved;
        }
        return stubTauri(cmd, args);
      });
      render(
        <>
          <PanePanelsMount />
          <RightPaneLayout repoPath="/repo" onClose={() => {}} />
        </>,
      );
      await waitFor(() => expect(getPanel(source.panelId)).toBeDefined());
      await act(async () => {
        await setSurfaceDashboard(ORG, "notes", sourceRef, true);
      });
      expect(useDashboardProfileStore.getState().profile?.rightPaneSurfaces?.notes).toEqual(
        copyRef,
      );
      recovered = true;
      await emit("pane-changed", { scope: "private", org: ORG, id: copyRef.id });
      await waitFor(() =>
        expect(document.querySelector("iframe")?.src).toContain(`${copyRef.id}.html`),
      );
      expect(
        findPanelLocation(useViewerStore.getState().rightPaneLayout, copy.panelId),
      ).not.toBeNull();
    },
  );

  it.each([
    { overlap: false, watcher: false },
    { overlap: true, watcher: false },
    { overlap: true, watcher: true },
  ])(
    "swaps Notes in place through flush (overlap=$overlap, watcher=$watcher)",
    async ({ overlap, watcher }) => {
      const ref = { scope: "private" as const, org: ORG, id: "notes" };
      const panelId = `pane:private:${ORG}/notes`;
      const entry: PaneEntry = {
        id: "notes",
        title: "Notes",
        scope: "private",
        surface: "notes",
        dashboardRef: ref,
        panelId,
        path: `${ROOT}/-/panes/notes.html`,
        panesDir: `${ROOT}/-/panes`,
      };
      const inboxRef = { ...ref, id: "inbox" };
      const inbox = {
        ...entry,
        id: "inbox",
        panelId: `pane:private:${ORG}/inbox`,
        surface: "human-inbox",
        dashboardRef: inboxRef,
        path: `${ROOT}/-/panes/inbox.html`,
      };
      const profile = {
        id: "default",
        name: "Default",
        posture: "",
        slots: {},
        rightPaneSurfaces: { notes: ref, ...(overlap ? { inbox: inboxRef } : {}) },
        cliLayout: "default",
        chrome: {},
      };
      disk.set("-/notes", "<title>Notes</title>");
      if (overlap) disk.set("-/inbox", "<title>Inbox</title>");
      usePaneStore.setState({ teamKey: null, listing: null, knownEntries: {} });
      useDashboardProfileStore.setState({ org: ORG, profile });
      useViewerStore.setState({
        notesLoadedScope: { githubUserId: 0, orgSlug: ORG },
        scratchNoteLoadStatus: "loaded",
        notes: ["alpha", "beta", "gamma", "previous"].map((id) => ({
          id,
          title: id,
          pinned: false,
          lastOpenedAt: 1,
        })),
        activeNoteId: "previous",
        rightPaneOpen: true,
        rightPaneLayout: overlap
          ? revealPanel(defaultRightPaneLayout(), INBOX_PANEL_ID)
          : defaultRightPaneLayout(),
        setActiveNote: async (id) => {
          useViewerStore.setState({ activeNoteId: id });
        },
      });
      sourceTransport = new PaneSourceProjection(
        () => ({
          contentRoot: `${ROOT}/${TEAM}/content`,
          chain: [TEAM],
          panes: overlap ? [entry, inbox] : [entry],
        }),
        (event, payload) => {
          for (const cb of listeners.get(event) ?? []) cb({ payload });
        },
      );
      tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd === "list_panes") return sourceTransport.read(args ?? {});
        if (cmd === "write_dashboard_profile")
          return { ...profile, ...(args?.patch as Record<string, unknown>) };
        if (cmd === "live_hub_view_key") return "all";
        if (cmd === "load_app_state") return "{}";
        return stubTauri(cmd, args);
      });
      await liveHub.writeUi("ui:right-pane", "all", { noteId: "alpha" });
      render(
        <>
          <PanePanelsMount />
          <RightPaneLayout repoPath="/repo" onClose={() => {}} />
        </>,
      );
      const docs: ReturnType<typeof bootPaneDoc>[] = [];
      const boot = async () => {
        const notesFrame = () =>
          [...document.querySelectorAll("iframe")].find((frame) =>
            frame.src.includes("notes.html"),
          );
        await waitFor(() => expect(notesFrame()).toBeDefined());
        const page = bootPaneDoc(
          notesFrame()!,
          `<output id="selection"></output><script>
        codesense.subscribe("ui:right-pane", { onChange: s => document.getElementById("selection").textContent = s.snapshot[0]?.noteId || "" });
        codesense.ready();
      </script>`,
          { origin: `http://f-test.localhost:${PORT}`, token: "frame-token" },
        );
        docs.push(page);
        return page;
      };
      try {
        const page = await boot();
        await waitFor(() =>
          expect(page.doc.getElementById("selection")?.textContent).toBe("alpha"),
        );
        await act(async () => {
          await page.win.codesense.setUiState("ui:right-pane", { noteId: "beta" });
        });
        let release!: () => void;
        const flushing = new Promise<void>((resolve) => {
          release = resolve;
        });
        const flush = vi.fn(() => flushing);
        page.win.codesense.onFlush(flush);
        await act(async () => {
          await setSurfaceDashboard(ORG, "notes", ref, false);
        });
        await waitFor(() => expect(flush).toHaveBeenCalledOnce());
        expect(
          findPanelLocation(useViewerStore.getState().rightPaneLayout, panelId),
        ).not.toBeNull();
        if (watcher) {
          const requests = tauriInvoke.mock.calls.filter(([cmd]) => cmd === "list_panes").length;
          await emit("pane-changed", { scope: "private", org: ORG, id: "notes" });
          await waitFor(() =>
            expect(
              Object.values(usePaneStore.getState().sourceRows).find(
                (row) => row.entry?.panelId === panelId,
              )?.sourceRevision,
            ).toBeGreaterThan(0),
          );
          expect(tauriInvoke.mock.calls.filter(([cmd]) => cmd === "list_panes")).toHaveLength(
            requests,
          );
        }
        await act(async () => {
          release();
        });
        await screen.findByTestId("scratch-note-panel");
        await waitFor(() => expect(useViewerStore.getState().activeNoteId).toBe("beta"));
        const classic = findPanelLocation(useViewerStore.getState().rightPaneLayout, "scratch");
        expect(classic).not.toBeNull();
        if (overlap) {
          expect(
            findPanelLocation(useViewerStore.getState().rightPaneLayout, INBOX_PANEL_ID),
          ).not.toBeNull();
          expect(
            findPanelLocation(useViewerStore.getState().rightPaneLayout, inbox.panelId),
          ).toBeNull();
        }
        act(() => useViewerStore.setState({ activeNoteId: "gamma" }));
        await waitFor(async () =>
          expect((await liveHub.snapshot("ui:right-pane", "all")).snapshot[0]).toMatchObject({
            noteId: "gamma",
          }),
        );
        await act(async () => {
          await setSurfaceDashboard(ORG, "notes", ref, true);
        });
        const next = await boot();
        await waitFor(() =>
          expect(next.doc.getElementById("selection")?.textContent).toBe("gamma"),
        );
        expect(findPanelLocation(useViewerStore.getState().rightPaneLayout, panelId)).toEqual(
          classic,
        );
        const notesBody = document
          .querySelector<HTMLIFrameElement>('iframe[src*="notes.html"]')!
          .closest<HTMLElement>('[data-testid="pane-body"]')!;
        expect(within(notesBody).queryByTestId("slot-fallback-strip")).not.toBeInTheDocument();
      } finally {
        for (const page of docs) await page.win.happyDOM.close();
      }
    },
  );

  it("keeps the default Inbox placement and a recovery body when its first listing fails", async () => {
    const saved = defaultRightPaneLayout();
    useViewerStore.setState({ rightPaneLayout: saved });
    tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "list_panes") throw new Error("offline");
      return stubTauri(cmd, args);
    });
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );
    expect(await screen.findByText("Your messages are safe.")).toBeInTheDocument();
    expect(useViewerStore.getState().rightPaneLayout).toEqual(saved);
    expect(screen.getByTestId("slot-fallback-strip")).toHaveTextContent("Inbox pane didn't load.");
  });

  it.each(["weft-board", BOARD])(
    "a built-in-only listing replaces a saved Inbox in place with active tab %s",
    async (activePanelId) => {
      const inbox: PaneEntry = {
        id: "inbox",
        scope: "private",
        dashboardRef: { scope: "built-in", org: ORG, id: "inbox" },
        declaration: { slots: ["right-pane"], surface: "human-inbox", capabilities: [] },
        surface: "human-inbox",
        title: "Inbox",
        path: "/h/users/42/builtin-panes/panes/inbox.html",
        panesDir: "/h/users/42/builtin-panes/panes",
        panelId: `pane:private:${ORG}/inbox`,
      };
      const saved = {
        sections: [
          {
            id: "notes-first",
            group: { panelIds: ["scratch"], activePanelId: "scratch" },
            size: 0.28,
            collapsed: false,
          },
          {
            id: "saved-inbox",
            group: { panelIds: [BOARD, "weft-board"], activePanelId },
            size: 0.72,
            collapsed: true,
            label: "Messages",
          },
        ],
      };
      let landListing!: (value: PaneListing) => void;
      const pending = new Promise<PaneListing>((resolve) => (landListing = resolve));
      tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) =>
        cmd === "list_panes" ? pending : stubTauri(cmd, args),
      );
      useViewerStore.setState({
        rightPaneLayout: reconcileLayoutToBuiltins(
          migrateLegacyRightPaneState({ rightPaneLayout: saved }),
        ),
        rightPaneOpen: true,
      });
      render(
        <>
          <PanePanelsMount />
          <RightPaneLayout repoPath="/repo" onClose={() => {}} />
        </>,
      );
      expect(useViewerStore.getState().rightPaneLayout.sections[1].group.panelIds).toContain(
        INBOX_PANEL_ID,
      );
      await act(async () => {
        landListing({ contentRoot: `${ROOT}/${TEAM}/content`, chain: [TEAM], panes: [inbox] });
      });
      await waitFor(() => expect(getPanel(inbox.panelId)).toBeDefined());
      expect(useViewerStore.getState().rightPaneLayout).toEqual({
        sections: [
          saved.sections[0],
          {
            ...saved.sections[1],
            group: {
              panelIds: [BOARD, INBOX_PANEL_ID],
              activePanelId: activePanelId === "weft-board" ? INBOX_PANEL_ID : activePanelId,
            },
          },
        ],
      });
      expect(resolveSlot("right-pane", { surface: "inbox" })).toMatchObject({
        dashboard: inbox,
        panelId: INBOX_PANEL_ID,
      });
      expect(resolveSlot("right-pane", { dashboard: inbox.dashboardRef! })).toMatchObject({
        dashboard: inbox,
        panelId: inbox.panelId,
      });
    },
  );

  it("keeps an arbitrary-id Inbox's name and native surface after its file is deleted", async () => {
    const panel = `pane:private:${ORG}/triage`;
    disk.set(
      "-/triage",
      '<title>Triage</title><meta name="codesense:surface" content="human-inbox">',
    );
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );
    await waitFor(() => expect(getPanel(panel)).toBeDefined());
    act(() => useViewerStore.getState().revealPanelById(panel));
    await waitFor(() =>
      expect(document.querySelector("iframe")?.getAttribute("src")).toContain("triage.html"),
    );
    disk.delete("-/triage");
    await emit("pane-changed", { scope: "private", org: ORG, id: "triage" });
    await waitFor(() =>
      expect(screen.getByTestId("slot-fallback-strip")).toHaveTextContent(
        "Triage pane didn't load.",
      ),
    );
    expect(
      await within(screen.getByTestId("pane-body")).findByText("Your messages are safe."),
    ).toBeInTheDocument();
    expect(getPanel(panel)?.title).toBe("Triage · Private");
  });
  it("file save and restore keep the failure, native body and attempt count until the next page is live", async () => {
    const panel = `pane:private:${ORG}/triage`;
    disk.set(
      "-/triage",
      '<title>Triage</title><meta name="codesense:surface" content="human-inbox">',
    );
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );
    await waitFor(() => expect(getPanel(panel)).toBeDefined());
    act(() => useViewerStore.getState().revealPanelById(panel));
    await waitFor(() => expect(document.querySelector("iframe")?.src).toContain("triage.html"));
    disk.delete("-/triage");
    await emit("pane-changed", { scope: "private", org: ORG, id: "triage" });
    const strip = await screen.findByTestId("slot-fallback-strip");
    const native = await within(screen.getByTestId("pane-body")).findByText(
      "Your messages are safe.",
    );
    fireEvent.click(within(strip).getByRole("button", { name: "Details" }));
    fireEvent.click(within(strip).getByRole("button", { name: "Reload" }));
    await waitFor(() =>
      expect(within(strip).getByRole("button", { name: "Reload" })).toBeInTheDocument(),
    );
    expect(strip).toHaveTextContent("2 times");
    disk.set(
      "-/triage",
      '<title>Triage</title><meta name="codesense:surface" content="human-inbox"><p>Fixed</p>',
    );
    await emit("pane-changed", { scope: "private", org: ORG, id: "triage" });
    // The frame is remounted once its mint resolves; capture the minted one.
    await waitFor(() => expect(document.querySelector("iframe")?.src).toContain("triage.html"));
    expect(screen.getByTestId("slot-fallback-strip")).toBe(strip);
    expect(within(screen.getByTestId("pane-body")).getByText("Your messages are safe.")).toBe(
      native,
    );
    expect(strip).toHaveTextContent("3 times");
    const previousFrame = document.querySelector("iframe");
    disk.set(
      "-/triage",
      '<title>Triage</title><meta name="codesense:surface" content="human-inbox"><p>Saved again</p>',
    );
    await emit("pane-changed", { scope: "private", org: ORG, id: "triage" });
    await waitFor(() => expect(document.querySelector("iframe")).not.toBe(previousFrame));
    expect(screen.getByTestId("slot-fallback-strip")).toBe(strip);
    expect(within(screen.getByTestId("pane-body")).getByText("Your messages are safe.")).toBe(
      native,
    );
    await waitFor(() => expect(strip).toHaveTextContent("4 times"));
    await waitFor(() => expect(document.querySelector("iframe")?.src).toContain("triage.html"));
    const frame = document.querySelector("iframe")!;
    fireEvent.load(frame);
    act(() =>
      window.dispatchEvent(
        new PaneMessageEvent("message", {
          source: frame.contentWindow,
          data: { type: "codesense:live" },
        }),
      ),
    );
    expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
  });
  it.each([
    { manual: "success", automatic: "success" },
    { manual: "success", automatic: "failure" },
    { manual: "failure", automatic: "success" },
    { manual: "failure", automatic: "failure" },
  ])(
    "a $manual Reload does not move terminal focus on a later $automatic watcher save",
    async ({ manual, automatic }) => {
      const panel = `pane:private:${ORG}/focus`;
      const html =
        '<title>Focus inbox</title><meta name="codesense:surface" content="human-inbox">';
      disk.set("-/focus", html);
      render(
        <>
          <input aria-label="Terminal" />
          <PanePanelsMount />
          <RightPaneLayout repoPath="/repo" onClose={() => {}} />
        </>,
      );
      await waitFor(() => expect(getPanel(panel)).toBeDefined());
      act(() => useViewerStore.getState().revealPanelById(panel));
      await waitFor(() => expect(document.querySelector("iframe")?.src).toContain("focus.html"));
      disk.delete("-/focus");
      await emit("pane-changed", { scope: "private", org: ORG, id: "focus" });
      const strip = await screen.findByTestId("slot-fallback-strip");
      const reload = within(strip).getByRole("button", { name: "Reload" });
      if (manual === "success") disk.set("-/focus", html);
      reload.focus();
      fireEvent.click(reload);
      const announce = (type: string) => {
        const frame = document.querySelector<HTMLIFrameElement>("[data-pane-current] iframe")!;
        fireEvent.load(frame);
        act(() =>
          window.dispatchEvent(
            new PaneMessageEvent("message", {
              source: frame.contentWindow,
              data: { type, kind: "error", message: "startup broke" },
            }),
          ),
        );
      };
      if (manual === "success") {
        await waitFor(() => expect(document.querySelector("iframe")?.src).toContain("focus.html"));
        announce("codesense:live");
        expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
        expect(document.activeElement).toHaveAttribute("data-panel-header-row");
      } else {
        await waitFor(() =>
          expect(within(strip).getByRole("button", { name: "Reload" })).toHaveFocus(),
        );
      }
      const terminal = screen.getByRole("textbox", { name: "Terminal" });
      terminal.focus();
      const previousFrame = document.querySelector("iframe");
      disk.set("-/focus", html + "<p>Saved</p>");
      await emit("pane-changed", { scope: "private", org: ORG, id: "focus" });
      await waitFor(() => {
        const candidate = document.querySelector<HTMLIFrameElement>("[data-pane-current] iframe");
        expect(candidate).toBeTruthy();
        expect(candidate).not.toBe(previousFrame);
      });
      await waitFor(() => expect(document.querySelector("iframe")?.src).toContain("focus.html"));
      announce(automatic === "success" ? "codesense:live" : "codesense:doc-error");
      if (automatic === "success") expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
      else expect(screen.getByTestId("slot-fallback-strip")).toBeInTheDocument();
      await waitFor(() => expect(terminal).toHaveFocus());
    },
  );
  it("shows the approved mismatch copy from the parser's actual rejected reason", async () => {
    disk.set(`${TEAM}/mismatch`, "<title>Mismatch</title>");
    const base = tauriInvoke.getMockImplementation()!;
    tauriInvoke.mockImplementation(async (cmd, args) => {
      const result = await base(cmd, args);
      return cmd === "list_panes"
        ? {
            ...result,
            sources: undefined,
            panes: result.panes.map((p: PaneEntry) => ({
              ...p,
              status: "rejected",
              rejectedReason: "manifest/meta mismatch",
            })),
          }
        : result;
    });
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );
    const panel = `pane:${ORG}:${TEAM}/mismatch`;
    await waitFor(() => expect(getPanel(panel)).toBeDefined());
    act(() => useViewerStore.getState().revealPanelById(panel));
    const strip = await screen.findByTestId("slot-fallback-strip");
    fireEvent.click(within(strip).getByRole("button", { name: "Details" }));
    expect(strip).toHaveTextContent(
      "Its settings file and the page disagree, so it was never opened.",
    );
    expect(document.querySelector("iframe")).toBeNull();
  });
  it("write → listed and offered; open → placed; edit → reloads; broken JS → banner; delete → placeholder, placement kept", async () => {
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );

    // 1. An agent writes panes/board.html: it is listed and offered in View → Panels.
    await agentWrites(TEAM, "board", "<title>Board</title><p>v1</p>");
    await waitFor(() => expect(getPanel(BOARD)?.title).toBe("Board"));
    expect(buildPanelMenuEntries()).toContainEqual({ id: BOARD, label: "Board", checked: false });

    // 2. pane_open places and reveals it, through the same layout as a built-in.
    await emit("pane-open", { panelId: BOARD, path: `${ROOT}/${TEAM}/panes/board.html` });
    await waitFor(() =>
      expect(findPanelLocation(useViewerStore.getState().rightPaneLayout, BOARD)).not.toBeNull(),
    );
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
    await waitFor(() => expect(paneFrame().getAttribute("src")).toContain("/pane/"));
    const firstFrame = paneFrame();
    const firstSrc = new URL(firstFrame.getAttribute("src")!);
    const firstSourceRevision = usePaneStore.getState().reloadKeys[BOARD];
    expect(firstSrc.pathname.endsWith("/board.html")).toBe(true);
    expect(firstSrc.searchParams.has("cap")).toBe(false);

    // 3. Saving the file reloads the pane with the new content.
    await agentWrites(TEAM, "board", "<title>Board</title><p>v2</p>");
    await waitFor(() =>
      expect(usePaneStore.getState().reloadKeys[BOARD]).toBeGreaterThan(firstSourceRevision),
    );
    await waitFor(() => {
      const current = document.querySelector<HTMLIFrameElement>("[data-pane-current] iframe");
      expect(current).toBeTruthy();
      expect(current).not.toBe(firstFrame);
      const src = current?.getAttribute("src");
      expect(src).toBeTruthy();
      expect(new URL(src!).searchParams.get("v")).not.toBe(firstSrc.searchParams.get("v"));
    });
    const candidate = document.querySelector<HTMLIFrameElement>("[data-pane-current] iframe")!;
    fireEvent.load(candidate);
    await act(async () => {
      window.dispatchEvent(
        new PaneMessageEvent("message", {
          data: { type: "codesense:live" },
          source: candidate.contentWindow,
        }),
      );
    });
    await waitFor(() => expect(paneFrame()).toBe(candidate));

    // 4. Broken JS: the shim forwards the uncaught error and the dev banner shows it;
    //    a rejected bridge call is echoed there too.
    const frame = candidate;
    const postMessage = vi.fn();
    const fakeWindow = { postMessage } as unknown as Window;
    Object.defineProperty(frame, "contentWindow", { value: fakeWindow, configurable: true });
    await act(async () => {
      window.dispatchEvent(
        new PaneMessageEvent("message", { data: { type: "codesense:live" }, source: fakeWindow }),
      );
    });
    await act(async () => {
      window.dispatchEvent(
        new PaneMessageEvent("message", {
          data: {
            type: "codesense:doc-error",
            kind: "error",
            message: "ReferenceError: chart is not defined",
          },
          source: fakeWindow,
        }),
      );
    });
    expect(screen.getByTestId("pane-dev-banner")).toHaveTextContent(
      "ReferenceError: chart is not defined",
    );
    await act(async () => {
      window.dispatchEvent(
        new PaneMessageEvent("message", {
          data: { type: "codesense:invoke", id: 1, command: "notAVerb", args: {} },
          source: fakeWindow,
        }),
      );
    });
    await waitFor(() => expect(postMessage).toHaveBeenCalled());
    const [reply] = postMessage.mock.calls[0];
    expect(reply).toMatchObject({ type: "codesense:invoke-result", id: 1, ok: false });
    await waitFor(() =>
      expect(screen.getByTestId("pane-dev-banner")).toHaveTextContent(
        `notAVerb: ${reply.error as string}`,
      ),
    );

    // The same failure left uncaught in the pane arrives as its unhandled
    // rejection too; it is shown once.
    const before = screen.getByTestId("pane-dev-banner").textContent;
    await act(async () => {
      window.dispatchEvent(
        new PaneMessageEvent("message", {
          data: { type: "codesense:doc-error", kind: "rejection", message: reply.error },
          source: fakeWindow,
        }),
      );
    });
    expect(screen.getByTestId("pane-dev-banner").textContent).toBe(before);

    // 5. Deleting the file shows the placeholder; the placement survives a layout
    //    reload (the load-time reconcile keeps `pane:` sections).
    await agentDeletes(TEAM, "board");
    await waitFor(() => expect(screen.getByTestId("slot-fallback-strip")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "Details" }));
    expect(screen.getByTestId("slot-fallback-strip")).toHaveTextContent(`${TEAM}/panes/board.html`);
    const layout = useViewerStore.getState().rightPaneLayout;
    expect(findPanelLocation(layout, BOARD)).not.toBeNull();
    const reloaded = reconcileLayoutToBuiltins(JSON.parse(JSON.stringify(layout)));
    expect(findPanelLocation(reloaded, BOARD)).not.toBeNull();
  });

  it("a pane that fails to load shows a banner with its path and the reason", async () => {
    disk.set(`${TEAM}/broken`, "<title>Broken</title>");
    mockFetch.mockResolvedValue({ ok: false, status: 500, statusText: "Internal Server Error" });
    const broken = `pane:${ORG}:${TEAM}/broken`;
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );
    await waitFor(() => expect(getPanel(broken)).toBeDefined());
    await act(async () => useViewerStore.getState().revealPanelById(broken));
    await waitFor(() => expect(screen.getByTestId("slot-fallback-strip")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "Details" }));
    expect(screen.getByTestId("slot-fallback-strip")).toHaveTextContent(
      `${ROOT}/${TEAM}/panes/broken.html: HTTP 500 Internal Server Error`,
    );
  });

  it("a pre-check that fails at the network level is not a load problem once the pane runs", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      disk.set(`${TEAM}/board`, "<title>Board</title>");
      // WebKit's TypeError for a fetch it refuses (e.g. a missing CORS header),
      // while the iframe's own GET loads fine.
      mockFetch.mockRejectedValue(new TypeError("Load failed"));
      render(
        <>
          <PanePanelsMount />
          <RightPaneLayout repoPath="/repo" onClose={() => {}} />
        </>,
      );
      await waitFor(() => expect(getPanel(BOARD)).toBeDefined());
      await act(async () => useViewerStore.getState().revealPanelById(BOARD));
      await waitFor(() => expect(paneFrame().getAttribute("src")).toContain("/board.html"));
      await waitFor(() => expect(mockFetch).toHaveBeenCalled());

      // The real page becomes live before the slot deadline. The failed HEAD
      // must not displace a working frame when its network grace expires.
      const frame = paneFrame();
      const win = { postMessage: vi.fn() } as unknown as Window;
      Object.defineProperty(frame, "contentWindow", { value: win, configurable: true });
      await act(async () => {
        window.dispatchEvent(
          new PaneMessageEvent("message", { data: { type: "codesense:ready" }, source: win }),
        );
        window.dispatchEvent(
          new PaneMessageEvent("message", { data: { type: "codesense:live" }, source: win }),
        );
        vi.advanceTimersByTime(NETWORK_PROBLEM_GRACE_MS);
      });
      expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
      expect(paneFrame()).toBe(frame);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a placed pane survives a switch to a team that doesn't show it, and is back on return", async () => {
    disk.set(`${TEAM}/board`, "<title>Board</title>");
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );
    await waitFor(() => expect(getPanel(BOARD)).toBeDefined());
    await act(async () => useViewerStore.getState().revealPanelById(BOARD));
    await waitFor(() => expect(paneFrame().getAttribute("src")).toContain("/pane/"));

    await act(async () => useOrgStore.setState({ activeTeamName: "design" }));
    await waitFor(() => expect(getPanel(BOARD)).toBeUndefined());
    expect(screen.queryByTestId("pane-body")).toBeNull();
    expect(findPanelLocation(useViewerStore.getState().rightPaneLayout, BOARD)).not.toBeNull();

    await act(async () => useOrgStore.setState({ activeTeamName: TEAM }));
    await waitFor(() => expect(paneFrame().getAttribute("src")).toContain("/board.html"));
  });

  it("a placement restored after the listing landed, whose file is gone, shows the placeholder", async () => {
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );
    await waitFor(() => expect(tauriInvoke).toHaveBeenCalledWith("list_panes", expect.anything()));
    const gone = `pane:${ORG}:${TEAM}/gone`;
    await act(async () => {
      useViewerStore.setState({
        rightPaneOpen: true,
        rightPaneLayout: revealPanel(genericPaneLayout(), gone),
      });
    });
    await waitFor(() => expect(screen.getByTestId("slot-fallback-strip")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "Details" }));
    expect(screen.getByTestId("slot-fallback-strip")).toHaveTextContent(`${TEAM}/panes/gone.html`);
  });

  it("a pane's context query answers for its team's content, as a dashboard there would", async () => {
    disk.set(`${TEAM}/board`, "<title>Board</title>");
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );
    await waitFor(() => expect(getPanel(BOARD)).toBeDefined());
    await act(async () => useViewerStore.getState().revealPanelById(BOARD));
    await waitFor(() => expect(paneFrame().getAttribute("src")).toContain("/pane/"));
    const frame = paneFrame();
    const fakeWindow = { postMessage: vi.fn() } as unknown as Window;
    Object.defineProperty(frame, "contentWindow", { value: fakeWindow, configurable: true });
    await act(async () => {
      window.dispatchEvent(
        new PaneMessageEvent("message", {
          data: { type: "codesense:invoke", id: 7, command: "queryContext", args: {} },
          source: fakeWindow,
        }),
      );
    });
    await waitFor(() =>
      expect(tauriInvoke).toHaveBeenCalledWith("query_context", {
        query: {},
        page: `${ROOT}/${TEAM}/content/board.html`,
        frameId: "test",
        callerPage: undefined,
        callerTeam: undefined,
      }),
    );
  });

  it("a pane whose listing fails shows the failure in place, not an endless loading state", async () => {
    disk.set(`${TEAM}/board`, "<title>Board</title>");
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );
    await waitFor(() => expect(getPanel(BOARD)).toBeDefined());
    await act(async () => useViewerStore.getState().revealPanelById(BOARD));
    await waitFor(() => expect(paneFrame().getAttribute("src")).toContain("/pane/"));

    tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "list_panes") throw new Error("org.yml unreadable");
      return stubTauri(cmd, args);
    });
    await act(async () => useOrgStore.setState({ activeTeamName: "design" }));
    await waitFor(() => expect(screen.getByTestId("slot-fallback-strip")).toBeVisible());
    fireEvent.click(screen.getByRole("button", { name: "Details" }));
    expect(screen.getByTestId("slot-fallback-strip")).toHaveTextContent(
      "pane list: org.yml unreadable",
    );
  });

  it("a parent team's pane is listed for its child team and not for an unrelated team", async () => {
    disk.set(`${PARENT}/risks`, "<title>Risks</title>");
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );
    const risks = `pane:${ORG}:${PARENT}/risks`;
    await waitFor(() => expect(getPanel(risks)?.title).toBe(`Risks · ${PARENT}`));

    await act(async () => useOrgStore.setState({ activeTeamName: "design" }));
    await waitFor(() => expect(getPanel(risks)).toBeUndefined());
    // pane_open for a pane this team doesn't show places nothing: an
    // unregistered placement would be invisible and unremovable.
    await emit("pane-open", { panelId: risks, path: "" });
    await new Promise((r) => setTimeout(r, 20));
    expect(findPanelLocation(useViewerStore.getState().rightPaneLayout, risks)).toBeNull();
  });
});

describe("human-inbox pane bridge (seam)", () => {
  const secondTeam = { org: ORG, team: PARENT };
  const rollup = {
    messages: [
      { id: 1, team: { org: ORG, team: TEAM }, text: "first" },
      { id: 2, team: secondTeam, text: "second" },
    ],
    roles: [{ role: "reviewer", holder: null, pending: 2, note: "no live holder" }],
    updates: [{ team: secondTeam, text: "working" }],
  };

  async function openPane(owner: string, marked: boolean) {
    disk.set(
      `${owner}/inbox`,
      `<title>Inbox</title>${marked ? '<meta name="codesense:surface" content="human-inbox">' : ""}`,
    );
    tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "live_hub_view_key") return "all";
      if (cmd === "live_hub_subscribe" && args?.topic !== "paneSources")
        return { topic: args?.topic, viewKey: args?.viewKey, rev: 0, snapshot: [] };
      if (cmd === "live_hub_unsubscribe") return null;
      if (cmd === "list_inbox_rollup") return rollup;
      if (cmd === "reply_as_human") return { status: "sent", messageId: 3 };
      if (cmd === "mark_inbox_messages_read") return (args?.messageIds as unknown[]).length;
      if (cmd === "bridge_doc_context") return { org: ORG, team: TEAM, layer: "team" };
      return stubTauri(cmd, args);
    });
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );
    const panelId = owner === "-" ? `pane:private:${ORG}/inbox` : `pane:${ORG}:${owner}/inbox`;
    await waitFor(() => expect(getPanel(panelId)).toBeTruthy());
    await emit("pane-open", { panelId, path: `${ROOT}/${owner}/panes/inbox.html` });
    await waitFor(() => expect(paneFrame().getAttribute("src")).toBeTruthy());
    const frame = paneFrame();
    const postMessage = vi.fn();
    const win = { postMessage } as unknown as Window;
    Object.defineProperty(frame, "contentWindow", { value: win, configurable: true });
    await act(async () => {
      window.dispatchEvent(
        new PaneMessageEvent("message", { source: win, data: { type: "codesense:ready" } }),
      );
    });
    let id = 0;
    async function call(command: string, args?: unknown) {
      const requestId = ++id;
      await act(async () => {
        window.dispatchEvent(
          new PaneMessageEvent("message", {
            source: win,
            data: { type: "codesense:invoke", id: requestId, command, args },
          }),
        );
      });
      await waitFor(() =>
        expect(
          postMessage.mock.calls.some(
            ([m]) => m.type === "codesense:invoke-result" && m.id === requestId,
          ),
        ).toBe(true),
      );
      return postMessage.mock.calls.find(
        ([m]) => m.type === "codesense:invoke-result" && m.id === requestId,
      )![0];
    }
    return { call, postMessage };
  }

  it("carries the discovered surface through the host, reads both teams and writes into the row's team", async () => {
    const page = await openPane("-", true);
    expect(await page.call("inboxRollup")).toMatchObject({ ok: true, data: rollup });
    expect(
      await page.call("replyAsHuman", { ...secondTeam, messageId: 2, text: "yes" }),
    ).toMatchObject({ ok: true, data: { status: "sent" } });
    expect(tauriInvoke).toHaveBeenCalledWith(
      "reply_as_human",
      expect.objectContaining({ team: secondTeam, messageId: 2 }),
    );
    expect(await page.call("markRead", { ...secondTeam, messageIds: [2] })).toMatchObject({
      ok: true,
      data: { marked: 1 },
    });
    expect(tauriInvoke).toHaveBeenCalledWith("mark_inbox_messages_read", {
      team: secondTeam,
      messageIds: [2],
    });
    await page.call("markRead", { messageIds: [1] });
    expect(tauriInvoke).toHaveBeenCalledWith("mark_inbox_messages_read", {
      team: { org: ORG, team: TEAM },
      messageIds: [1],
    });

    // Run the shipped shim's subscription against the host's actual push.
    const source = readFileSync(
      resolve(__dirname, "../../../../../src-tauri/src/http/routes/viewer.rs"),
      "utf8",
    );
    const subscribe =
      source.match(/onInboxChanged:(function\(cb\).*?)},context:function/)![1] + "}";
    const pageListeners = new Set<(e: unknown) => void>();
    const parent = {};
    const pageWindow = {
      parent,
      addEventListener: (_: string, cb: (e: unknown) => void) => pageListeners.add(cb),
      removeEventListener: (_: string, cb: (e: unknown) => void) => pageListeners.delete(cb),
    };
    const onInboxChanged = new Function("window", `return (${subscribe})`)(pageWindow);
    const changed = vi.fn();
    const off = onInboxChanged(changed);
    page.postMessage.mockImplementation((data) => {
      for (const cb of pageListeners) cb({ source: parent, data });
    });
    await emit("agent-inbox-changed", undefined);
    expect(changed).toHaveBeenCalledOnce();
    off();
    await emit("agent-inbox-changed", undefined);
    expect(changed).toHaveBeenCalledOnce();
  });

  it.each([
    [TEAM, true],
    ["-", false],
  ])(
    "allows a team pane with the tag or a private pane without it under open (%s, %s)",
    async (owner, marked) => {
      const page = await openPane(owner, marked);
      tauriInvoke.mockClear();
      expect(await page.call("inboxRollup")).toMatchObject({
        ok: true,
      });
      expect(await page.call("markRead", { ...secondTeam, messageIds: [2] })).toMatchObject({
        ok: true,
      });
      expect(
        tauriInvoke.mock.calls.some(
          ([cmd]) => cmd === "list_inbox_rollup" || cmd === "mark_inbox_messages_read",
        ),
      ).toBe(true);
    },
  );
});

function routingInbox(kind: "built-in" | "team" | "private", team = TEAM): PaneEntry {
  const scope = kind === "built-in" ? "private" : kind;
  return {
    id: "inbox",
    title: "Inbox",
    scope,
    team: scope === "team" ? team : undefined,
    dashboardRef: { scope: kind, org: ORG, id: "inbox", ...(scope === "team" ? { team } : {}) },
    surface: "human-inbox",
    declaration: { slots: ["right-pane"], surface: "human-inbox", capabilities: [] },
    panelId: scope === "team" ? `pane:${ORG}:${team}/inbox` : `pane:private:${ORG}/inbox`,
    path:
      kind === "built-in"
        ? "/h/users/42/builtin-panes/panes/inbox.html"
        : `${ROOT}/${team}/panes/inbox.html`,
    panesDir: "/panes",
  };
}
function routingHost(layout = defaultRightPaneLayout()) {
  mockFetch.mockResolvedValue({ ok: true, status: 200, statusText: "" });
  useViewerStore.setState({ rightPaneLayout: layout });
  render(
    <TooltipProvider>
      <PanePanelsMount />
      <RightPaneLayout repoPath="/repo" onClose={() => {}} />
    </TooltipProvider>,
  );
}
function openRoutingInbox(entry: "reveal" | "shortcut") {
  act(() => {
    if (entry === "reveal") revealSlot("right-pane", { surface: "inbox" });
    else SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-inbox-rollup")!.run();
  });
}
function routingProfile(ref: import("@/tauri/commands").DashboardRef) {
  return {
    id: "default",
    name: "Default",
    posture: "",
    slots: { "right-pane": [ref] },
    cliLayout: "default",
    chrome: {},
  };
}

describe("stable Inbox hosting", () => {
  it.each([false, true])(
    "selects the listed Inbox, honoring a valid custom profile (%s)",
    async (custom) => {
      const inbox = routingInbox("team");
      const triage = {
        ...routingInbox("private"),
        id: "triage",
        title: "Triage",
        panelId: `pane:private:${ORG}/triage`,
        path: "/panes/triage.html",
        dashboardRef: { scope: "private" as const, org: ORG, id: "triage" },
      };
      const profile = routingProfile(
        custom ? triage.dashboardRef : { scope: "team", org: ORG, team: TEAM, id: "notes" },
      );
      useDashboardProfileStore.setState({ org: ORG, profile });
      tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd === "list_panes")
          return { contentRoot: `${ROOT}/${TEAM}/content`, chain: [TEAM], panes: [inbox, triage] };
        return stubTauri(cmd, args);
      });
      routingHost();
      await waitFor(() => expect(usePaneStore.getState().listing?.panes).toHaveLength(2));
      openRoutingInbox("reveal");
      expect(resolveSlot("right-pane", { surface: "inbox" })).toMatchObject({
        panelId: INBOX_PANEL_ID,
        dashboard: custom ? triage : inbox,
      });
      expect(useViewerStore.getState().rightPaneLayout.sections[0].group.activePanelId).toBe(
        INBOX_PANEL_ID,
      );
      expect(buildPanelMenuEntries().filter((entry) => entry.label === "Inbox")).toHaveLength(1);
    },
  );

  it.each(["reveal", "shortcut"] as const)(
    "%s opens a failed discovery in the saved section and Reload preserves it",
    async (entry) => {
      let reject!: (error: Error) => void;
      const pending = new Promise<PaneListing>((_, fail) => {
        reject = fail;
      });
      const inbox = routingInbox("built-in");
      const profile = routingProfile({ scope: "team", org: ORG, team: "old-team", id: "inbox" });
      useDashboardProfileStore.setState({ org: ORG, profile });
      let recovered = false;
      tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd === "list_panes")
          return recovered
            ? { contentRoot: `${ROOT}/${TEAM}/content`, chain: [TEAM], panes: [inbox] }
            : pending;
        return stubTauri(cmd, args);
      });
      const saved = {
        sections: [
          {
            id: "notes-first",
            group: { panelIds: ["scratch"], activePanelId: "scratch" },
            collapsed: false,
            size: 0.23,
          },
          {
            id: "mail",
            label: "Messages",
            group: { panelIds: [BOARD, INBOX_PANEL_ID], activePanelId: BOARD },
            collapsed: true,
            size: 0.77,
          },
        ],
      };
      routingHost(saved);
      openRoutingInbox(entry);
      await act(async () => reject(new Error("offline")));
      expect(await screen.findByText("Your messages are safe.")).toBeInTheDocument();
      const opened = useViewerStore.getState().rightPaneLayout;
      expect(opened).toEqual({
        sections: [
          saved.sections[0],
          {
            ...saved.sections[1],
            collapsed: false,
            group: { ...saved.sections[1].group, activePanelId: INBOX_PANEL_ID },
          },
        ],
      });
      act(() =>
        revealSlot("right-pane", {
          surface: "inbox",
          intent: { conversation: "#qa", org: ORG, team: TEAM },
        }),
      );
      recovered = true;
      fireEvent.click(screen.getByRole("button", { name: "Reload" }));
      await waitFor(() =>
        expect(resolveSlot("right-pane", { surface: "inbox" }).dashboard).toEqual(inbox),
      );
      expect(useViewerStore.getState().rightPaneLayout).toEqual(opened);
      expect(usePaneStore.getState().pendingIntents[INBOX_PANEL_ID]).toMatchObject({
        conversation: "#qa",
      });
      expect(useViewerStore.getState().rightPaneOpen).toBe(true);
    },
  );

  it.each([
    ["built-in", "team", false],
    ["team", "private", false],
    ["private", "built-in", false],
    ["built-in", "team", true],
  ] as const)(
    "changes %s → %s behind the same section (team switch: %s)",
    async (from, to, switchTeam) => {
      let current = routingInbox(from);
      useDashboardProfileStore.setState({ org: ORG, profile: null });
      sourceTransport = new PaneSourceProjection(
        (_org, team) => ({
          contentRoot: `${ROOT}/${team}/content`,
          chain: [team],
          panes: [current],
        }),
        (event, payload) => {
          for (const cb of listeners.get(event) ?? []) cb({ payload });
        },
      );
      routingHost();
      await waitFor(() =>
        expect(resolveSlot("right-pane", { surface: "inbox" }).dashboard).toEqual(current),
      );
      if (switchTeam) openRoutingInbox("reveal");
      const saved = {
        sections: [
          {
            id: "notes-first",
            group: { panelIds: ["scratch"], activePanelId: "scratch" },
            collapsed: false,
            size: 0.28,
          },
          {
            id: "mail",
            label: "Messages",
            group: {
              panelIds: [BOARD, INBOX_PANEL_ID],
              activePanelId: switchTeam ? INBOX_PANEL_ID : BOARD,
            },
            collapsed: !switchTeam,
            size: 0.72,
          },
        ],
      };
      act(() => useViewerStore.getState().setRightPaneLayout(saved));
      current = routingInbox(to, switchTeam ? "web" : TEAM);
      if (switchTeam) act(() => useOrgStore.setState({ activeTeamName: "web" }));
      else
        await emit("pane-changed", {
          scope: to === "private" ? "private" : "team",
          org: ORG,
          team: TEAM,
          id: "inbox",
        });
      await waitFor(() =>
        expect(resolveSlot("right-pane", { surface: "inbox" }).dashboard).toEqual(current),
      );
      expect(useViewerStore.getState().rightPaneLayout).toEqual(saved);
      expect(buildPanelMenuEntries().filter((entry) => entry.label === "Inbox")).toHaveLength(1);
      openRoutingInbox("reveal");
      expect(useViewerStore.getState().rightPaneLayout.sections).toHaveLength(2);
    },
  );

  it.each(["restore", "pane_open", "Panels menu", "team switch"] as const)(
    "%s leaves an independent Inbox-surface dashboard alone",
    async (entry) => {
      let inbox = routingInbox("team");
      const triage = {
        ...routingInbox("private"),
        id: "triage",
        title: "Triage",
        panelId: `pane:private:${ORG}/triage`,
        path: "/panes/triage.html",
        dashboardRef: { scope: "private" as const, org: ORG, id: "triage" },
      };
      useDashboardProfileStore.setState({ org: ORG, profile: null });
      tauriInvoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd === "list_panes")
          return {
            contentRoot: `${ROOT}/${args?.team}/content`,
            chain: [String(args?.team)],
            panes: [inbox, triage],
          };
        return stubTauri(cmd, args);
      });
      const canonical = {
        id: "mail",
        label: "Messages",
        group: { panelIds: [INBOX_PANEL_ID], activePanelId: INBOX_PANEL_ID },
        collapsed: true,
        size: 0.65,
      };
      const saved = {
        sections: [
          {
            id: "notes-first",
            group: { panelIds: ["scratch"], activePanelId: "scratch" },
            collapsed: false,
            size: 0.35,
          },
          canonical,
          ...(["restore", "team switch"].includes(entry)
            ? [
                {
                  id: "triage",
                  group: { panelIds: [triage.panelId], activePanelId: triage.panelId },
                  collapsed: false,
                  size: 0.4,
                },
              ]
            : []),
        ],
      };
      routingHost(saved);
      await waitFor(() => expect(getPanel(triage.panelId)).toBeDefined());
      if (entry === "pane_open")
        await emit("pane-open", { panelId: triage.panelId, path: triage.path });
      else if (entry === "Panels menu") act(() => handlePanelMenuAction(`panel:${triage.panelId}`));
      else if (entry === "team switch") {
        inbox = routingInbox("team", "web");
        act(() => useOrgStore.setState({ activeTeamName: "web" }));
        await waitFor(() => expect(usePaneStore.getState().teamKey).toBe(`${ORG}:web`));
      }
      const layout = useViewerStore.getState().rightPaneLayout;
      expect(findPanelLocation(layout, triage.panelId)).not.toBeNull();
      expect(layout.sections.find((section) => section.id === "mail")).toEqual(canonical);
      expect(resolveSlot("right-pane", { surface: "inbox" }).panelId).toBe(INBOX_PANEL_ID);
    },
  );
});

it.each([false, true])(
  "completes saved installed-copy migration after failed metadata discovery (opened=%s)",
  async (opened) => {
    const copy = {
      ...listing(TEAM).panes[0],
      id: "my-copy",
      scope: "private" as const,
      title: "My Inbox",
      panelId: `pane:private:${ORG}/my-copy`,
      path: `${ROOT}/-/panes/my-copy.html`,
      panesDir: `${ROOT}/-/panes`,
      dashboardRef: {
        scope: "private" as const,
        org: ORG,
        id: "my-copy",
        installedFrom: { scope: "built-in" as const, org: ORG, id: "inbox" },
      },
      surface: "human-inbox",
    };
    const triage = {
      ...copy,
      id: "triage",
      panelId: `pane:private:${ORG}/triage`,
      dashboardRef: { scope: "private" as const, org: ORG, id: "triage" },
      path: `${ROOT}/-/panes/triage.html`,
    };
    const saved = {
      sections: [
        {
          id: "notes-first",
          group: { panelIds: ["scratch"], activePanelId: "scratch" },
          size: 0.23,
          collapsed: false,
        },
        {
          id: "saved-mail",
          label: "My messages",
          group: { panelIds: [triage.panelId, copy.panelId], activePanelId: copy.panelId },
          size: 0.77,
          collapsed: true,
        },
      ],
    };
    let recovered = false;
    sourceTransport = new PaneSourceProjection(
      () => {
        if (!recovered) throw new Error("offline metadata");
        return { contentRoot: `${ROOT}/${TEAM}/content`, chain: [TEAM], panes: [copy, triage] };
      },
      (event, payload) => {
        for (const cb of listeners.get(event) ?? []) cb({ payload });
      },
    );
    const { loadSavedInboxPanelIds } = await import("../inbox-layout-migration");
    const inboxPanelIds = await loadSavedInboxPanelIds(saved, `${ORG}:${TEAM}`);
    useViewerStore.setState({
      rightPaneLayout: migrateLegacyRightPaneState({ rightPaneLayout: saved, inboxPanelIds }),
    });
    render(
      <TooltipProvider>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </TooltipProvider>,
    );
    await waitFor(() =>
      expect(usePaneStore.getState().listError?.message).toBe("offline metadata"),
    );
    if (opened) openRoutingInbox("shortcut");
    recovered = true;
    if (!opened) openRoutingInbox("shortcut");
    fireEvent.click(await screen.findByRole("button", { name: "Reload" }));
    await waitFor(() => expect(usePaneStore.getState().teamKey).toBe(`${ORG}:${TEAM}`));
    await waitFor(() =>
      expect(findPanelLocation(useViewerStore.getState().rightPaneLayout, copy.panelId)).toBeNull(),
    );
    expect(useViewerStore.getState().rightPaneLayout).toEqual({
      sections: [
        saved.sections[0],
        {
          ...saved.sections[1],
          group: { panelIds: [triage.panelId, INBOX_PANEL_ID], activePanelId: INBOX_PANEL_ID },
        },
      ],
    });
    openRoutingInbox("shortcut");
    expect(
      useViewerStore
        .getState()
        .rightPaneLayout.sections.flatMap((s) => s.group.panelIds)
        .filter((id) => id === INBOX_PANEL_ID),
    ).toHaveLength(1);
    expect(
      findPanelLocation(useViewerStore.getState().rightPaneLayout, triage.panelId),
    ).not.toBeNull();
  },
);

it.each(["dashboard", "team"] as const)(
  "clears the Inbox badge on a %s change and ignores its retiring reader",
  async (change) => {
    const a: PaneEntry = {
      id: "triage-a",
      title: "First Inbox",
      scope: "private",
      surface: "human-inbox",
      panelId: `pane:private:${ORG}/triage-a`,
      path: `${ROOT}/-/panes/triage-a.html`,
      panesDir: `${ROOT}/-/panes`,
      dashboardRef: { scope: "private", org: ORG, id: "triage-a" },
    };
    const b: PaneEntry = {
      ...a,
      id: "triage-b",
      title: "Second Inbox",
      panelId: `pane:private:${ORG}/triage-b`,
      path: `${ROOT}/-/panes/triage-b.html`,
      dashboardRef: { scope: "private", org: ORG, id: "triage-b" },
    };
    let profile = {
      id: "default",
      name: "Default",
      posture: "",
      slots: { "right-pane": a.dashboardRef! },
      cliLayout: "default",
      chrome: {},
    };
    const records = new Map<string, ReturnType<typeof frameFixture>>();
    let nonce = 0;
    tauriInvoke.mockImplementation(async (cmd, args: any) => {
      if (cmd === "write_dashboard_profile") {
        profile = { ...profile, ...args.patch };
        return profile;
      }
      if (cmd === "list_panes")
        return {
          contentRoot: `${ROOT}/${args.team}/content`,
          chain: [args.team],
          panes: change === "team" && args.team !== TEAM ? [b] : [a, b],
        };
      if (cmd === "mint_frame") {
        const record = frameFixture(args.args, PORT);
        record.frameId = `badge-${++nonce}`;
        record.origin = `http://f-${record.frameId}.localhost:${PORT}`;
        record.token = record.frameId;
        records.set(record.origin, record);
        return record;
      }
      if (cmd === "resolve_frame_grant") return records.get(args.origin)?.grant;
      return stubTauri(cmd, args);
    });
    useDashboardProfileStore.setState({ org: ORG, profile });
    usePaneStore.setState({ badges: {} });
    useViewerStore.setState({ rightPaneLayout: defaultRightPaneLayout(), rightPaneOpen: true });
    mockFetch.mockResolvedValue({ ok: true, status: 200 });
    render(
      <>
        <PanePanelsMount />
        <RightPaneLayout repoPath="/repo" onClose={() => {}} />
      </>,
    );
    const frameFor = async (name: string) =>
      waitFor(() => {
        const frame = [...document.querySelectorAll("iframe")].find((f) => f.src.includes(name));
        expect(frame).toBeDefined();
        return frame!;
      });
    const firstFrame = await frameFor("triage-a.html");
    const record = records.get(new URL(firstFrame.src).origin)!;
    const first = bootPaneDoc(
      firstFrame,
      "<script>codesense.onFlush(() => new Promise(resolve => window.finishFlush = resolve)); codesense.ready();</script>",
      record,
    );
    try {
      await act(async () => {
        await first.win.codesense.setBadge({ count: 7, severity: "info" });
      });
      await waitFor(() => expect(usePaneStore.getState().badges[INBOX_PANEL_ID]?.count).toBe(7));
      if (change === "dashboard")
        await act(async () => {
          await setSurfaceDashboard(ORG, "inbox", b.dashboardRef!, true);
        });
      else act(() => useOrgStore.setState({ activeTeamName: "web" }));
      await frameFor("triage-b.html");
      expect(firstFrame.isConnected).toBe(true);
      await waitFor(() => expect(usePaneStore.getState().badges[INBOX_PANEL_ID]).toBeUndefined());
      await act(async () => {
        await first.win.codesense.setBadge({ count: 99, severity: "warning" });
      });
      expect(usePaneStore.getState().badges[INBOX_PANEL_ID]).toBeUndefined();
      await act(async () => {
        first.win.eval("window.finishFlush?.()");
      });
    } finally {
      await first.win.happyDOM.close();
    }
  },
);

it("the native panes menu excludes tab-only built-ins and opens eligible bundled panes", async () => {
  const builtins: PaneEntry[] = [
    ["inbox", "src/features/shell/right-pane/__fixtures__/inbox-pane.html"],
    ["agent-list", "src/features/shell/slots/__fixtures__/agent-list-pane.html"],
    ["files-tree", "src/features/shell/slots/builtin/files-tree.html"],
    ["story-trail", "src-tauri/assets/dashboards/story-trail/index.html"],
  ].map(([id, source]) => {
    const html = readFileSync(resolve(source), "utf8");
    const slots =
      /name="codesense:slots" content="([^"]*)"/.exec(html)?.[1].split(/\s+/).filter(Boolean) ?? [];
    return {
      id,
      title: /<title>(.*?)<\/title>/.exec(html)?.[1] ?? id,
      scope: "private",
      dashboardRef: { scope: "built-in", org: ORG, id },
      declaration: {
        slots,
        capabilities: [],
        ...(id === "inbox" ? { surface: "human-inbox" } : {}),
      },
      ...(id === "inbox" ? { surface: "human-inbox" } : {}),
      path: `/h/users/42/builtin-panes/panes/${id}.html`,
      panesDir: "/h/users/42/builtin-panes/panes",
      panelId: `pane:private:${ORG}/${id}`,
    } as PaneEntry;
  });
  const result = { contentRoot: `${ROOT}/${TEAM}/content`, chain: [TEAM], panes: builtins };
  sourceTransport = new PaneSourceProjection(
    () => result,
    (event, payload) => {
      for (const cb of listeners.get(event) ?? []) cb({ payload });
    },
  );
  useDashboardProfileStore.setState({
    org: ORG,
    profile: {
      id: "default",
      name: "Default",
      posture: "",
      slots: {},
      cliLayout: "default",
      chrome: {},
    },
  });
  useViewerStore.setState({ rightPaneLayout: genericPaneLayout(), rightPaneOpen: true });
  mockFetch.mockResolvedValue({ ok: true, status: 200 });
  render(
    <>
      <PanePanelsMount />
      <RightPaneLayout repoPath="/repo" onClose={() => {}} />
    </>,
  );
  const files = builtins.find((pane) => pane.id === "files-tree")!;
  await waitFor(() => expect(getPanel(files.panelId)).toBeDefined());
  const menu = buildPanelMenuEntries();
  expect(
    menu.some((row) => row.id === builtins.find((pane) => pane.id === "story-trail")!.panelId),
  ).toBe(false);
  expect(
    menu.some((row) => row.id === builtins.find((pane) => pane.id === "agent-list")!.panelId),
  ).toBe(false);
  expect(menu.some((row) => row.id === files.panelId)).toBe(true);
  expect(menu.some((row) => row.id === INBOX_PANEL_ID)).toBe(true);
  for (const [panelId, id] of [
    [INBOX_PANEL_ID, "inbox"],
    [files.panelId, "files-tree"],
  ]) {
    await act(async () => {
      handlePanelMenuAction("panel:" + panelId);
    });
    await waitFor(() =>
      expect(
        tauriInvoke.mock.calls.some(
          ([cmd, args]) =>
            cmd === "mint_frame" &&
            args?.args?.documentPath === builtins.find((pane) => pane.id === id)!.path &&
            args.args.slot === "right-pane",
        ),
      ).toBe(true),
    );
    expect(
      [...document.querySelectorAll("iframe")].some((frame) => frame.src.includes(id + ".html")),
    ).toBe(true);
  }
});
