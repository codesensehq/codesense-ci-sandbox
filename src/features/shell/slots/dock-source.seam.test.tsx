// SPDX-License-Identifier: MIT
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { frameIpc, frameEventClass } from "../../../../tests/support/fixtures/frame";

const { invoke, listeners } = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, Set<(e: { payload: unknown }) => void>>(),
}));
vi.mock("@tauri-apps/api/core", async (original) => ({
  ...(await original<typeof import("@tauri-apps/api/core")>()),
  invoke,
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, cb: (e: { payload: unknown }) => void) => {
    const set = listeners.get(event) ?? new Set();
    listeners.set(event, set);
    set.add(cb);
    return () => set.delete(cb);
  }),
  emit: vi.fn(async () => {}),
}));
vi.mock("@/features/dashboard/lib/verb-dispatch", async () =>
  (await import("@/test/verb-dispatch-mock")).openVerbDispatchMock(),
);
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));
vi.mock("@/features/viewer/components/agents-list", () => ({
  AgentsList: () => <div>Built-in Agents</div>,
}));

import { PaneSourceProjection } from "@/app/mock/pane-source-projection";
import { DockSlotHost } from "./dock-slot-host";
import { FileNavSlotHost, FileNavSlotMount } from "./file-nav-slot-host";
import { PanePanelsMount } from "../right-pane/panes/use-register-pane-panels";
import { useOrgStore } from "@/shared/stores/org-store";
import { usePaneStore } from "../right-pane/panes/pane-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useDashboardProfileStore } from "@/features/dashboard/lib/dashboard-profile";
import { dashboardRefKey } from "@/features/dashboard/lib/dashboard-ref";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { useDashboardCommandsSync } from "@/features/dashboard/lib/dashboard-commands";
import type { FrameRecord, PaneEntry } from "@/tauri/commands";
import { TooltipProvider } from "@/components/ui/tooltip";
import { switchContext } from "@/features/viewer/lib/context-switch";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { HostMenuContext, type HostMenuRequest } from "../host-menu-context";

describe.each(["dock", "file-nav"] as const)("%s source revisions", (slot) => {
  const ref = { scope: "team" as const, org: "acme", team: "a", id: "people" };
  const profile = {
    id: "default",
    name: "Default",
    posture: "",
    slots: { [slot]: ref },
    cliLayout: "default",
    chrome: {},
  };
  let title = "People";
  let modified = "2026-10-23T12:00:00.000Z";
  let chord = "Alt+P";
  let present = true;
  let dropDelta = false;
  let readerScopedRevision = false;
  let projectedRevisionByTeam = new Map<string, number>();
  let transport: PaneSourceProjection;
  let activeFrame: FrameRecord | null = null;
  let activeToken = "";
  let tokenSerial = 0;
  let revokedTokens: string[] = [];
  let rebindTeams: string[] = [];
  let rebindOrganizations: string[] = [];
  let identityDenials = 0;
  let failPrimaryMint = false;
  let releasedFrames: string[] = [];
  function entry(): PaneEntry {
    return {
      dashboardRef: ref,
      id: ref.id,
      scope: "team",
      team: "a",
      title,
      modified,
      declaration: {
        slots: [slot],
        capabilities: [],
        commands: [{ id: "find", label: `Find ${title}`, defaultChord: chord, scope: "global" }],
      },
      path: "/ctx/a/panes/people.html",
      panesDir: "/ctx/a/panes",
      panelId: "pane:acme:a/people",
    };
  }
  function builtinAgentList(): PaneEntry {
    return {
      dashboardRef: { scope: "built-in", org: "acme", id: "agent-list" },
      id: "agent-list",
      title: "Agents",
      scope: "private",
      path: "/builtin/panes/agent-list.html",
      panesDir: "/builtin/panes",
      panelId: "pane:private:acme/agent-list",
      declaration: { slots: ["dock"], capabilities: [] },
      status: "ready",
    } as PaneEntry;
  }
  function Commands() {
    useDashboardCommandsSync();
    return null;
  }
  function Root() {
    const team = useOrgStore((s) => s.activeTeamName);
    return (
      <TooltipProvider>
        <PanePanelsMount />
        <Commands />
        {slot === "dock" ? (
          <DockSlotHost teamKey={`acme:${team}`} visible />
        ) : (
          <>
            <FileNavSlotMount teamKey={`acme:${team}`}>
              <div>Built-in Agents</div>
            </FileNavSlotMount>
            <FileNavSlotHost />
          </>
        )}
      </TooltipProvider>
    );
  }
  const frame = () =>
    document.querySelector<HTMLIFrameElement>(
      `[data-slot-id="${slot}"] [data-pane-current] iframe`,
    );
  function collapse(collapsed: boolean) {
    const team = useOrgStore.getState().activeTeamName;
    useViewerStore.setState({
      workspacePanelCollapsed: collapsed,
      navCollapsed: { [`acme:${team}`]: collapsed },
    });
  }
  async function mount() {
    render(<Root />);
    await waitFor(() => expect(frame()?.src).toContain("people.html"));
    const event = frameEventClass(9321);
    await act(async () => {
      fireEvent.load(frame()!);
      window.dispatchEvent(
        new event("message", {
          source: frame()!.contentWindow,
          data: { type: "codesense:live", token: activeToken },
        }),
      );
    });
  }
  function connectShim(frame: HTMLIFrameElement) {
    let pageToken = activeFrame?.token ?? activeToken;
    const postMessage = vi.fn((message: { type: string; token?: string }) => {
      if (message.type === "codesense:rebind") {
        pageToken = message.token ?? pageToken;
        return;
      }
      if (message.type !== "codesense:rebind-pending") return;
      queueMicrotask(() => {
        window.dispatchEvent(
          new (frameEventClass(9321))("message", {
            source: frame.contentWindow,
            data: { type: "codesense:rebind-ack", token: pageToken },
          }),
        );
      });
    });
    const endpoint = { postMessage };
    Object.defineProperty(frame, "contentWindow", { value: endpoint, configurable: true });
    return {
      postMessage,
      ready() {
        window.dispatchEvent(
          new (frameEventClass(9321))("message", {
            source: endpoint as unknown as Window,
            data: { type: "codesense:ready", token: pageToken },
          }),
        );
      },
      /** The page's own `cs.ready()`, with the token the page currently holds. */
      live() {
        window.dispatchEvent(
          new (frameEventClass(9321))("message", {
            source: endpoint as unknown as Window,
            data: { type: "codesense:live", token: pageToken },
          }),
        );
      },
    };
  }
  it("a command sent while the same-org dock rebinds is delivered once the rebind lands", async () => {
    if (slot !== "dock") return;
    useOrgStore.setState({
      orgs: [{ slug: "acme", teams: [{ name: "a" }, { name: "b" }] }] as never,
      activeOrgSlug: "acme",
      activeTeamName: "a",
    });
    warmScopes.activate("acme:a");
    const mintIpc = invoke.getMockImplementation()!;
    invoke.mockImplementation(async (cmd, args) => {
      const result = await mintIpc(cmd, args);
      if (cmd === "mint_frame" && (args as any)?.args?.documentPath?.endsWith("people.html"))
        result.grant.dashboard = dashboardRefKey(ref);
      return result;
    });
    render(<Root />);
    await waitFor(() => expect(frame()?.src).toContain("people.html"));
    const original = frame()!;
    const shim = connectShim(original);
    await act(async () => {
      fireEvent.load(original);
      shim.ready();
      shim.live();
    });
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    let rebinding = false;
    const prior = invoke.getMockImplementation()!;
    invoke.mockImplementation(async (cmd, args) => {
      if (cmd === "rebind_frame_team") {
        rebinding = true;
        await gate;
      }
      return prior(cmd, args);
    });
    await act(async () => {
      expect(switchContext({ teamKey: "acme:b" }, "switcher")).toBe(true);
    });
    await waitFor(() => expect(rebinding).toBe(true));
    expect(warmScopes.tierOf("acme:a")).toBe("warm");
    const { runDashboardCommand } = await import("@/features/dashboard/lib/dashboard-commands");
    const command = commands().find((c: any) => c.slot === "dock" && c.command === "find");
    expect(command).toBeDefined();
    act(() => runDashboardCommand(command.id));
    await act(async () => release());
    await waitFor(() => expect(activeFrame?.grant.team?.team).toBe("b"));
    await waitFor(() =>
      expect(
        shim.postMessage.mock.calls.filter(
          ([m]) => m.type === "codesense:command" && (m as any).id === "find",
        ),
      ).toHaveLength(1),
    );
  });
  async function change(id: string | undefined = "people") {
    await act(async () => transport.changed({ scope: "team", org: "acme", team: "a", id }));
  }
  const commands = () =>
    invoke.mock.calls.filter(([cmd]) => cmd === "commands_sync").at(-1)?.[1].commands ?? [];
  beforeEach(() => {
    liveHub.dispose();
    listeners.clear();
    title = "People";
    modified = "2026-10-23T12:00:00.000Z";
    chord = "Alt+P";
    present = true;
    dropDelta = false;
    readerScopedRevision = false;
    projectedRevisionByTeam = new Map();
    activeFrame = null;
    activeToken = "";
    tokenSerial = 0;
    revokedTokens = [];
    rebindTeams = [];
    rebindOrganizations = [];
    identityDenials = 0;
    failPrimaryMint = false;
    releasedFrames = [];
    invoke.mockReset();
    vi.stubGlobal(
      "ResizeObserver",
      class {
        observe() {}
        disconnect() {}
      },
    );
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      x: 0,
      y: 0,
      left: 0,
      top: 0,
      right: 240,
      bottom: 400,
      width: 240,
      height: 400,
      toJSON() {},
    });
    transport = new PaneSourceProjection(
      (_org, team) => ({
        contentRoot: `/ctx/${team}/content`,
        chain: [team],
        panes: [
          ...(team === "a" && present ? [entry()] : []),
          ...(slot === "dock" ? [builtinAgentList()] : []),
        ],
      }),
      (event, payload) => {
        if (event === "codesense:delta" && dropDelta) return;
        for (const cb of listeners.get(event) ?? []) cb({ payload });
      },
    );
    invoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      const frame = frameIpc(cmd, args, 9321);
      if (cmd === "mint_frame" && frame) {
        const mintArgs = args?.args as { contentRoot?: string; documentPath?: string };
        if (failPrimaryMint && mintArgs.documentPath?.endsWith("people.html"))
          throw new Error("people pane cannot be served");
        const team = mintArgs.contentRoot?.match(/\/ctx\/([^/]+)\/content/)?.[1];
        return (frame as Promise<FrameRecord>).then((record) => {
          activeToken = `mint-${++tokenSerial}`;
          activeFrame = {
            ...record,
            token: activeToken,
            grant: { ...record.grant, team: team ? { org: "acme", team } : null },
          };
          return activeFrame;
        });
      }
      if (cmd === "release_frame") {
        releasedFrames.push(String(args?.frameId));
        return;
      }
      if (cmd === "rebind_frame_team") {
        if (args?.token !== activeToken || !activeFrame) {
          identityDenials += 1;
          throw new Error("denied: frame identity");
        }
        revokedTokens.push(activeToken);
        const team = String(args?.team);
        rebindTeams.push(team);
        rebindOrganizations.push(String(args?.org));
        activeToken = `rebind-${team}-${++tokenSerial}`;
        activeFrame = {
          ...activeFrame,
          token: activeToken,
          grant: { ...activeFrame.grant, team: { org: String(args?.org), team } },
        };
        return activeFrame;
      }
      if (cmd === "resolve_frame_grant") {
        if (args?.token !== activeToken || !activeFrame) {
          identityDenials += 1;
          throw new Error("denied: frame identity");
        }
        return activeFrame.grant;
      }
      if (cmd === "live_hub_view_key") {
        if (args?.token && args.token !== activeToken) {
          identityDenials += 1;
          throw new Error("denied: frame identity");
        }
        return activeFrame?.grant.team
          ? `team:${activeFrame.grant.team.org}/${activeFrame.grant.team.team}`
          : "all";
      }
      if (cmd === "live_hub_subscribe" && args?.topic !== "paneSources")
        return { topic: args?.topic, viewKey: args?.viewKey, rev: 0, snapshot: [] };
      if (frame !== undefined) return frame;
      if (cmd === "read_dashboard_profile") return profile;
      if (cmd === "get_http_api_port") return 9321;
      if (cmd === "list_builtin_panes") {
        const team = String(args?.team ?? "b");
        return {
          contentRoot: `/ctx/${team}/content`,
          chain: [team],
          panes: [builtinAgentList()],
        };
      }
      if (cmd === "list_panes") return transport.read(args ?? {});
      if (cmd === "live_hub_subscribe" && args?.topic === "paneSources") {
        const snapshot = transport.snapshot(String(args.viewKey));
        if (!readerScopedRevision || !snapshot) return snapshot;
        const readerTeam = useOrgStore.getState().activeTeamName ?? "";
        const revision =
          projectedRevisionByTeam.get(readerTeam) ?? (projectedRevisionByTeam.size + 1) * 1000;
        projectedRevisionByTeam.set(readerTeam, revision);
        return {
          ...snapshot,
          snapshot: snapshot.snapshot.map((row) =>
            row.id === "view"
              ? row
              : { ...row, sourceRevision: Number(row.sourceRevision ?? 0) + revision },
          ),
        };
      }
      return null;
    });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({ ok: true, status: 200 })),
    );
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "b" });
    useDashboardProfileStore.setState({ org: "acme", profile });
    collapse(false);
  });
  afterEach(() => {
    cleanup();
    liveHub.dispose();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("refreshes the chosen sibling team's dock through pane ingress without Reload", async () => {
    render(<Root />);
    await waitFor(() => expect(frame()?.src).toContain("people.html"));
    const original = frame()!;
    const before = original.src;
    title = "Team members";
    chord = "Alt+M";
    await act(async () => {
      await transport.changed({ scope: "team", org: "acme", team: "a", id: "people" });
    });
    await waitFor(() => {
      expect(frame()?.src).toContain("people.html");
      expect(frame()?.src).not.toBe(before);
    });
    expect(frame()?.title).toBe("Team members");
    expect(
      invoke.mock.calls.some(
        ([cmd, args]) =>
          cmd === "commands_sync" &&
          args.commands.some((c: { label: string }) => c.label === "Find Team members"),
      ),
    ).toBe(true);
    expect(commands()[0].binding).toBe("Alt+M");
    expect(listeners.get("pane-changed")?.size ?? 0).toBe(0);
  });

  if (slot === "dock") {
    it("reloads once when only the source document/assets revision advances", async () => {
      await mount();
      const initialMints = invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame").length;
      const id = dashboardRefKey(ref);
      usePaneStore.setState((s) => ({
        sourceRows: {
          ...s.sourceRows,
          [id]: {
            ...s.sourceRows[id],
            sourceRevision: (s.sourceRows[id]?.sourceRevision ?? 0) + 1,
            documentRevision: (s.sourceRows[id]?.documentRevision ?? 0) + 1,
          },
        },
      }));
      await waitFor(() => {
        expect(invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame")).toHaveLength(
          initialMints + 1,
        );
      });
      await act(async () => Promise.resolve());
      expect(invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame")).toHaveLength(
        initialMints + 1,
      );
    });
  }

  if (slot === "dock") {
    it("reloads the pane document when its own modified time changes", async () => {
      await mount();
      const mints = invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame").length;
      modified = "2026-10-23T12:00:01.000Z";
      await change();
      await waitFor(() =>
        expect(invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame")).toHaveLength(mints + 1),
      );
    });
  }

  it("holds a hidden frame's source and catches up once on reveal", async () => {
    await mount();
    const original = frame()!;
    await act(async () => collapse(true));
    const mints = invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame").length;
    const fetches = vi.mocked(fetch).mock.calls.length;
    title = "Intermediate";
    await change();
    title = "Latest";
    await change();
    await waitFor(() => expect(commands()[0]?.dashboardTitle).toBe("Latest"));
    expect(frame()).toBe(original);
    expect(original.title).toBe("People");
    expect(vi.mocked(fetch).mock.calls.length).toBe(fetches);
    expect(invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame")).toHaveLength(mints);
    await act(async () => collapse(false));
    await waitFor(() => expect(frame()?.title).toBe("Latest"));
    await waitFor(() =>
      expect(invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame")).toHaveLength(mints + 1),
    );
  });

  it("uses the built-in agent-list frame on deletion and returns when the selected file reappears", async () => {
    if (slot !== "dock") return;
    await mount();
    vi.useFakeTimers();
    await act(async () => {
      usePaneStore
        .getState()
        .setListing(
          "acme:b",
          { contentRoot: "/ctx/b/content", chain: ["b"], panes: [builtinAgentList()] },
          {},
        );
      expect(usePaneStore.getState().teamKey).toBe("acme:b");
      expect(usePaneStore.getState().listing?.panes.some((pane) => pane.id === "agent-list")).toBe(
        true,
      );
      expect(usePaneStore.getState().sourceRows).toEqual({});
      await vi.advanceTimersByTimeAsync(3_000);
    });
    vi.useRealTimers();
    await waitFor(() => expect(frame()?.title).toBe("Agents"));
    expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
    expect(screen.queryByText("Built-in Agents")).toBeNull();
    expect(commands()).toEqual([]);
    const mintsBeforeReturn = invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame").length;
    await act(async () => {
      title = "Returned";
      present = true;
      await usePaneStore.getState().refresh?.(ref);
    });
    expect(usePaneStore.getState().sourceRows[dashboardRefKey(ref)]?.entry?.title).toBe("Returned");
    expect(
      usePaneStore.getState().sourceRows[dashboardRefKey(ref)]?.documentRevision,
    ).toBeGreaterThan(0);
    await waitFor(() =>
      expect(invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame")).toHaveLength(
        mintsBeforeReturn + 1,
      ),
    );
    const returnedFrame = await waitFor(() => {
      const minted = document.querySelector<HTMLIFrameElement>(
        '[data-pane-current] iframe[src*="people.html"]',
      );
      expect(minted).toBeTruthy();
      return minted;
    });
    const returnedShim = connectShim(returnedFrame!);
    await act(async () => {
      fireEvent.load(returnedFrame!);
      returnedShim.ready();
      returnedShim.live();
    });
    await waitFor(() => expect(frame()?.title).toBe("Returned"));
    expect(commands()[0].dashboardTitle).toBe("Returned");
  });

  it("recovers a revision gap through the real hub without listing again", async () => {
    await mount();
    const lists = invoke.mock.calls.filter(([cmd]) => cmd === "list_panes").length;
    dropDelta = true;
    title = "Missed";
    await change();
    dropDelta = false;
    title = "Recovered";
    await change();
    await waitFor(() => expect(frame()?.title).toBe("Recovered"));
    expect(
      invoke.mock.calls.some(
        ([cmd, args]) => cmd === "live_hub_subscribe" && args.topic === "paneSources",
      ),
    ).toBe(true);
    expect(invoke.mock.calls.filter(([cmd]) => cmd === "list_panes")).toHaveLength(lists);
  });

  it.each([
    ["a", "b"],
    ["b", "c"],
  ])("preserves the frame and source root switching %s to %s", async (from, to) => {
    useOrgStore.setState({ activeTeamName: from });
    await mount();
    const original = frame()!;
    const url = original.src;
    const mints = invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame").length;
    await act(async () => useOrgStore.setState({ activeTeamName: to }));
    await waitFor(() => expect(usePaneStore.getState().teamKey).toBe(`acme:${to}`));
    expect(frame()).toBe(original);
    expect(original.src).toBe(url);
    expect(invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame")).toHaveLength(mints);
  });

  if (slot === "dock") {
    it("falls back to the built-in agent list when a chosen dock frame never becomes live", async () => {
      vi.useFakeTimers();
      render(<Root />);
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(frame()?.src).toContain("people.html");
      expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3_000);
      });
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(frame()?.src).toContain("agent-list.html");
      expect(screen.queryByTestId("slot-fallback-strip")).toBeNull();
      expect(screen.queryByText("Built-in Agents")).toBeNull();
      vi.useRealTimers();
    });

    it("releases the fallback frame when the current attempt reports late page liveness", async () => {
      vi.useFakeTimers();
      render(<Root />);
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });
      const selected = frame()!;
      fireEvent.load(selected);
      const shim = connectShim(selected);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3_000);
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeTruthy();
      const fallbackFrame = document.querySelector<HTMLIFrameElement>(
        '[data-slot-fallback-entry="agent-list"] iframe',
      );
      expect(fallbackFrame?.src).toContain("agent-list.html");
      const releasesBeforeRecovery = releasedFrames.length;
      await act(async () => {
        shim.live();
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeNull();
      expect(releasedFrames.length).toBeGreaterThan(releasesBeforeRecovery);
      vi.useRealTimers();
    });

    it("falls back when only the automatic shim handshake arrives and the page never goes live", async () => {
      vi.useFakeTimers();
      render(<Root />);
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });
      const selected = frame()!;
      fireEvent.load(selected);
      const shim = connectShim(selected);
      await act(async () => shim.ready());
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3_000);
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeTruthy();
      vi.useRealTimers();
    });

    it("does not spend the startup budget while the document is hidden", async () => {
      vi.useFakeTimers();
      render(<Root />);
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(frame()?.src).toContain("people.html");
      Object.defineProperty(document, "visibilityState", {
        configurable: true,
        get: () => "hidden",
      });
      await act(async () => document.dispatchEvent(new Event("visibilitychange")));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(10_000);
      });
      expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeNull();
      Reflect.deleteProperty(document, "visibilityState");
      await act(async () => document.dispatchEvent(new Event("visibilitychange")));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(3_000);
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeTruthy();
      vi.useRealTimers();
    });

    it("keeps the shown frame until its replacement is live when a source save follows a team switch", async () => {
      useOrgStore.setState({
        orgs: [{ slug: "acme", teams: [{ name: "a" }, { name: "b" }] }] as never,
        activeTeamName: "a",
        activeOrgSlug: "acme",
      });
      const sourceId = dashboardRefKey(ref);
      const row = {
        id: sourceId,
        dashboardRef: ref,
        entry: entry(),
        contentRoot: "/ctx/a/content",
        sourceRevision: 0,
        documentRevision: 0,
        listed: true,
      };
      usePaneStore
        .getState()
        .setListing(
          "acme:a",
          { contentRoot: "/ctx/a/content", chain: ["a"], panes: [entry(), builtinAgentList()] },
          { [sourceId]: row },
        );
      await mount();
      const original = frame()!;
      const shim = connectShim(original);
      await act(async () => shim.ready());
      await act(async () => {
        expect(switchContext({ teamKey: "acme:b" }, "switcher")).toBe(true);
      });
      await waitFor(() => expect(activeFrame?.grant.team?.team).toBe("b"));
      const rebinds = invoke.mock.calls.filter(([cmd]) => cmd === "rebind_frame_team").length;
      await act(async () => {
        usePaneStore.getState().setListing(
          "acme:b",
          { contentRoot: "/ctx/b/content", chain: ["b"], panes: [entry(), builtinAgentList()] },
          {
            [sourceId]: {
              ...row,
              sourceRevision: 1,
              documentRevision: 1,
              contentRoot: "/ctx/b/content",
            },
          },
        );
      });
      expect(original.isConnected).toBe(true);
      expect(original.closest("[aria-hidden='true']")).toBeNull();
      await waitFor(() => {
        expect(frame()).not.toBe(original);
        expect(frame()?.src).toContain("people.html");
      });
      expect(original.isConnected).toBe(true);
      const replacement = frame()!;
      expect(
        invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame").at(-1)?.[1].args.contentRoot,
      ).toBe("/ctx/b/content");
      const replacementShim = connectShim(replacement);
      await act(async () => {
        fireEvent.load(replacement);
        replacementShim.ready();
        replacementShim.live();
      });
      await waitFor(() => expect(original.isConnected).toBe(false));
      expect(invoke.mock.calls.filter(([cmd]) => cmd === "rebind_frame_team")).toHaveLength(
        rebinds,
      );
    });

    it("mints a sibling-team pane against the active reader's content root", async () => {
      useOrgStore.setState({
        orgs: [{ slug: "acme", teams: [{ name: "a" }, { name: "b" }] }] as never,
        activeTeamName: "a",
        activeOrgSlug: "acme",
      });
      const sourceId = dashboardRefKey(ref);
      usePaneStore.getState().setListing(
        "acme:a",
        { contentRoot: "/ctx/a/content", chain: ["a"], panes: [entry(), builtinAgentList()] },
        {
          [sourceId]: {
            id: sourceId,
            dashboardRef: ref,
            entry: entry(),
            contentRoot: "/ctx/a/content",
            sourceRevision: 0,
            documentRevision: 0,
            listed: true,
          },
        },
      );
      await mount();
      const teamAFrame = frame();
      const mint = invoke.mock.calls.find(([cmd]) => cmd === "mint_frame")?.[1].args;
      expect(mint?.documentPath).toBe("/ctx/a/panes/people.html");
      expect(mint?.contentRoot).toBe("/ctx/a/content");
      await act(async () => {
        expect(switchContext({ teamKey: "acme:b" }, "switcher")).toBe(true);
      });
      await waitFor(() => expect(usePaneStore.getState().teamKey).toBe("acme:b"));
      const id = dashboardRefKey(ref);
      const prior = usePaneStore.getState().sourceRows[id];
      usePaneStore.getState().setListing(
        "acme:b",
        { contentRoot: "/ctx/b/content", chain: ["b"], panes: [entry(), builtinAgentList()] },
        {
          [id]: {
            ...prior,
            sourceRevision: prior.sourceRevision + 1,
            documentRevision: prior.documentRevision! + 1,
            contentRoot: "/ctx/b/content",
          },
        },
      );
      await waitFor(() => {
        const mintArgs = invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame").at(-1)?.[1].args;
        expect(mintArgs?.contentRoot).toBe("/ctx/b/content");
      });
      expect(invoke.mock.calls.filter(([cmd]) => cmd === "rebind_frame_team")).toHaveLength(0);
      // Talk to the frame that mint produced, not the pending one it replaces.
      await waitFor(() => {
        expect(frame()).not.toBe(teamAFrame);
        expect(frame()?.src).toContain("people.html");
      });
      const iframe = frame()!;
      await act(async () => {
        window.dispatchEvent(
          new (frameEventClass(9321))("message", {
            source: iframe.contentWindow,
            data: { type: "codesense:ready" },
          }),
        );
      });
      expect(invoke.mock.calls.filter(([cmd]) => cmd === "rebind_frame_team")).toHaveLength(0);
    });

    it("keeps one live dock frame through 50 rapid A↔B switches to team A's pane", async () => {
      readerScopedRevision = true;
      useOrgStore.setState({
        orgs: [{ slug: "acme", teams: [{ name: "a" }, { name: "b" }] }] as never,
        activeTeamName: "a",
        activeOrgSlug: "acme",
      });
      const sourceId = dashboardRefKey(ref);
      usePaneStore.getState().setListing(
        "acme:a",
        { contentRoot: "/ctx/a/content", chain: ["a"], panes: [entry(), builtinAgentList()] },
        {
          [sourceId]: {
            id: sourceId,
            dashboardRef: ref,
            entry: entry(),
            contentRoot: "/ctx/a/content",
            sourceRevision: 0,
            documentRevision: 0,
            listed: true,
          },
        },
      );
      await mount();
      const original = frame()!;
      const shim = connectShim(original);
      await act(async () => {
        shim.ready();
        shim.live();
      });
      await waitFor(() => expect(activeFrame?.grant.team?.team).toBe("a"));
      const initialMintCount = invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame").length;
      const initialSourceSubscriptionCount = new Set(
        invoke.mock.calls
          .filter(([cmd, args]) => cmd === "live_hub_subscribe" && args.topic === "paneSources")
          .map(([, args]) => String(args.subscriptionId)),
      ).size;
      expect(initialMintCount).toBeGreaterThan(0);

      for (let i = 0; i < 50; i += 1) {
        const team = i % 2 === 0 ? "b" : "a";
        await act(async () => {
          expect(switchContext({ teamKey: `acme:${team}` }, "switcher")).toBe(true);
        });
        await waitFor(() => expect(usePaneStore.getState().teamKey).toBe(`acme:${team}`));
        await waitFor(() => expect(activeFrame?.grant.team?.team).toBe(team));
        expect(rebindTeams.at(-1)).toBe(team);
        expect(
          shim.postMessage.mock.calls.filter(([m]) => m.type === "codesense:rebind"),
        ).toHaveLength(i + 1);
        expect(screen.queryByTestId("pane-startup-placeholder")).toBeNull();
        expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeNull();
        expect(frame()).toBe(original);
        expect(original.closest("[aria-hidden='true']")).toBeNull();
        expect(
          original.closest("[data-testid='agents-dock']")?.getAttribute("style") ?? "",
        ).not.toContain("display: none");
      }

      const sourceSubscribeIds = invoke.mock.calls
        .filter(([cmd, args]) => cmd === "live_hub_subscribe" && args.topic === "paneSources")
        .map(([, args]) => String(args.subscriptionId));
      const sourceUnsubscribeIds = new Set(
        invoke.mock.calls
          .filter(([cmd]) => cmd === "live_hub_unsubscribe")
          .map(([, args]) => String(args.subscriptionId)),
      );
      const activeSourceSubscriptions = new Set(
        sourceSubscribeIds.filter((id) => !sourceUnsubscribeIds.has(id)),
      );
      expect(activeSourceSubscriptions.size).toBe(initialSourceSubscriptionCount);

      expect(invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame")).toHaveLength(
        initialMintCount,
      );
      expect(invoke.mock.calls.filter(([cmd]) => cmd === "rebind_frame_team")).toHaveLength(50);
      expect(rebindTeams).toHaveLength(50);
      expect(activeFrame?.grant.team?.team).toBe("a");
      expect(new Set(revokedTokens).size).toBe(50);
      expect(revokedTokens).not.toContain(activeToken);
      expect(identityDenials).toBe(0);
    });

    it("keeps org A's dock page warm and bound to A across org A↔C, with no mint and no rebind", async () => {
      useOrgStore.setState({
        orgs: [
          { slug: "acme", teams: [{ name: "a" }] },
          { slug: "other", teams: [{ name: "c" }] },
        ] as never,
        activeTeamName: "a",
        activeOrgSlug: "acme",
      });
      warmScopes.activate("acme:a");
      const sourceId = dashboardRefKey(ref);
      usePaneStore.getState().setListing(
        "acme:a",
        { contentRoot: "/ctx/a/content", chain: ["a"], panes: [entry(), builtinAgentList()] },
        {
          [sourceId]: {
            id: sourceId,
            dashboardRef: ref,
            entry: entry(),
            contentRoot: "/ctx/a/content",
            sourceRevision: 0,
            documentRevision: 1, // the number the projection gives this document on its first read
            listed: true,
          },
        },
      );
      await mount();
      const original = frame()!;
      const shim = connectShim(original);
      await act(async () => shim.ready());
      const hidden = () => original.closest("[inert]") !== null;
      for (let trip = 0; trip < 3; trip += 1) {
        await act(async () => {
          expect(switchContext({ teamKey: "other:c" }, "switcher")).toBe(true);
        });
        // Org C has no dock choice: its own list shows, and A's page waits, hidden, still A's.
        expect(screen.getByText("Built-in Agents")).toBeTruthy();
        expect(original.isConnected).toBe(true);
        expect(hidden()).toBe(true);
        await act(async () => {
          expect(switchContext({ teamKey: "acme:a" }, "switcher")).toBe(true);
        });
        await waitFor(() => expect(hidden()).toBe(false));
        expect(frame()).toBe(original);
        expect(screen.queryByText("Built-in Agents")).toBeNull();
      }
      expect(rebindOrganizations).toEqual([]);
      expect(activeFrame?.grant.team).toEqual({ org: "acme", team: "a" });
      expect(invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame")).toHaveLength(1);
      expect(identityDenials).toBe(0);
    });

    it("mints an unchanged-source Reload for the active reader after a team switch", async () => {
      useOrgStore.setState({
        orgs: [{ slug: "acme", teams: [{ name: "a" }, { name: "b" }] }] as never,
        activeTeamName: "a",
        activeOrgSlug: "acme",
      });
      const sourceId = dashboardRefKey(ref);
      usePaneStore.getState().setListing(
        "acme:a",
        { contentRoot: "/ctx/a/content", chain: ["a"], panes: [entry(), builtinAgentList()] },
        {
          [sourceId]: {
            id: sourceId,
            dashboardRef: ref,
            entry: entry(),
            contentRoot: "/ctx/a/content",
            sourceRevision: 0,
            documentRevision: 0,
            listed: true,
          },
        },
      );
      const show = vi.fn(async (_request: HostMenuRequest) => ({ closed: true as const }));
      render(
        <HostMenuContext.Provider value={show}>
          <Root />
        </HostMenuContext.Provider>,
      );
      await waitFor(() => expect(frame()?.src).toContain("people.html"));
      const original = frame()!;
      const shim = connectShim(original);
      await act(async () => {
        fireEvent.load(original);
        shim.ready();
        shim.live();
      });
      await act(async () => {
        expect(switchContext({ teamKey: "acme:b" }, "switcher")).toBe(true);
      });
      await waitFor(() => expect(usePaneStore.getState().teamKey).toBe("acme:b"));
      await waitFor(() => expect(activeFrame?.grant.team?.team).toBe("b"));
      const mints = () => invoke.mock.calls.filter(([cmd]) => cmd === "mint_frame");
      const before = mints().length;
      const revisionBefore = usePaneStore.getState().sourceRows[sourceId].documentRevision;
      await act(async () =>
        window.dispatchEvent(
          new (frameEventClass(9321))("message", {
            source: original.contentWindow,
            data: {
              type: "codesense:contextmenu",
              token: activeToken,
              target: "frame",
              anchorRect: { x: 0, y: 0, width: 0, height: 0 },
            },
          }),
        ),
      );
      await waitFor(() => expect(show).toHaveBeenCalled());
      await act(async () => show.mock.calls.at(-1)![0].dashboard!.reload());
      await waitFor(() => expect(mints().length).toBeGreaterThan(before));
      expect(usePaneStore.getState().sourceRows[sourceId].documentRevision).toBe(revisionBefore);
      expect(mints()[before][1].args.contentRoot).toBe("/ctx/b/content");
    });

    it("keeps a live built-in fallback mounted and rebinds it through a same-org team switch", async () => {
      useOrgStore.setState({
        orgs: [{ slug: "acme", teams: [{ name: "a" }, { name: "b" }] }] as never,
        activeTeamName: "a",
        activeOrgSlug: "acme",
      });
      const sourceId = dashboardRefKey(ref);
      usePaneStore.getState().setListing(
        "acme:a",
        { contentRoot: "/ctx/a/content", chain: ["a"], panes: [entry(), builtinAgentList()] },
        {
          [sourceId]: {
            id: sourceId,
            dashboardRef: ref,
            entry: entry(),
            contentRoot: "/ctx/a/content",
            sourceRevision: 0,
            documentRevision: 0,
            listed: true,
          },
        },
      );
      failPrimaryMint = true;
      render(<Root />);
      const fallbackFrame = () =>
        document.querySelector<HTMLIFrameElement>('[data-slot-fallback-entry="agent-list"] iframe');
      // The frame is remounted once its mint resolves; capture the minted one.
      await waitFor(() => expect(fallbackFrame()?.src).toContain("agent-list.html"));
      const original = fallbackFrame()!;
      const shim = connectShim(original);
      await act(async () => {
        fireEvent.load(original);
        shim.ready();
        shim.live();
      });
      await screen.findByText("Agents pane loaded.");
      const listMints = () =>
        invoke.mock.calls.filter(
          ([cmd, args]) =>
            cmd === "mint_frame" && args.args.documentPath.endsWith("agent-list.html"),
        ).length;
      const mintsBefore = listMints();

      await act(async () => {
        expect(switchContext({ teamKey: "acme:b" }, "switcher")).toBe(true);
      });
      await waitFor(() => expect(usePaneStore.getState().teamKey).toBe("acme:b"));
      await waitFor(() => expect(activeFrame?.grant.team?.team).toBe("b"));
      expect(original.isConnected).toBe(true);
      expect(fallbackFrame()).toBe(original);
      expect(listMints()).toBe(mintsBefore);
      expect(rebindTeams).toEqual(["b"]);
      expect(identityDenials).toBe(0);
    });

    it("keeps org A's live built-in fallback mounted across an org switch and back", async () => {
      useOrgStore.setState({
        orgs: [
          { slug: "acme", teams: [{ name: "a" }] },
          { slug: "other", teams: [{ name: "c" }] },
        ] as never,
        activeTeamName: "a",
        activeOrgSlug: "acme",
      });
      warmScopes.activate("acme:a");
      const sourceId = dashboardRefKey(ref);
      usePaneStore.getState().setListing(
        "acme:a",
        { contentRoot: "/ctx/a/content", chain: ["a"], panes: [entry(), builtinAgentList()] },
        {
          [sourceId]: {
            id: sourceId,
            dashboardRef: ref,
            entry: entry(),
            contentRoot: "/ctx/a/content",
            sourceRevision: 0,
            documentRevision: 0,
            listed: true,
          },
        },
      );
      failPrimaryMint = true;
      render(<Root />);
      const fallbackFrame = () =>
        document.querySelector<HTMLIFrameElement>('[data-slot-fallback-entry="agent-list"] iframe');
      // The frame is remounted once its mint resolves; capture the minted one.
      await waitFor(() => expect(fallbackFrame()?.src).toContain("agent-list.html"));
      const original = fallbackFrame()!;
      const shim = connectShim(original);
      await act(async () => {
        fireEvent.load(original);
        shim.ready();
        shim.live();
      });
      await screen.findByText("Agents pane loaded.");
      await act(async () => {
        expect(switchContext({ teamKey: "other:c" }, "switcher")).toBe(true);
      });
      expect(original.isConnected).toBe(true);
      await act(async () => {
        expect(switchContext({ teamKey: "acme:a" }, "switcher")).toBe(true);
      });
      expect(fallbackFrame()).toBe(original);
    });

    it("keeps a starting org A built-in fallback bound to org A across an org switch", async () => {
      useOrgStore.setState({
        orgs: [
          { slug: "acme", teams: [{ name: "a" }] },
          { slug: "other", teams: [{ name: "c" }] },
        ] as never,
        activeTeamName: "a",
        activeOrgSlug: "acme",
      });
      warmScopes.activate("acme:a");
      const sourceId = dashboardRefKey(ref);
      usePaneStore.getState().setListing(
        "acme:a",
        { contentRoot: "/ctx/a/content", chain: ["a"], panes: [entry(), builtinAgentList()] },
        {
          [sourceId]: {
            id: sourceId,
            dashboardRef: ref,
            entry: entry(),
            contentRoot: "/ctx/a/content",
            sourceRevision: 0,
            documentRevision: 0,
            listed: true,
          },
        },
      );
      failPrimaryMint = true;
      render(<Root />);
      const fallbackFrame = () =>
        document.querySelector<HTMLIFrameElement>('[data-slot-fallback-entry="agent-list"] iframe');
      // The frame is remounted once its mint resolves; capture the minted one.
      await waitFor(() => expect(fallbackFrame()?.src).toContain("agent-list.html"));
      const original = fallbackFrame()!;
      const fallbackMints = () =>
        invoke.mock.calls.filter(
          ([cmd, payload]) =>
            cmd === "mint_frame" && payload.args.documentPath?.endsWith("agent-list.html"),
        );
      const before = fallbackMints().length;
      // Org A's fallback has not reported live yet when the other org comes on screen.
      await act(async () => {
        expect(switchContext({ teamKey: "other:c" }, "switcher")).toBe(true);
      });
      expect(original.isConnected).toBe(true);
      expect(fallbackMints().length).toBe(before);
      expect(
        fallbackMints().every(([, payload]) => payload.args.builtinTeam?.org !== "other"),
      ).toBe(true);
    });

    it("does not report a valid selected pane missing on a folded cold launch", async () => {
      collapse(true);
      render(<Root />);
      const sourceId = dashboardRefKey(ref);
      await waitFor(() =>
        expect(usePaneStore.getState().sourceRows[sourceId]?.entry?.path).toBe(entry().path),
      );
      await waitFor(() =>
        expect(usePaneStore.getState().listing?.contentRoot).toBe("/ctx/b/content"),
      );
      expect(screen.getByTestId("agents-dock").style.display).toBe("none");
      await act(async () => collapse(false));
      expect(document.querySelector('[data-slot-fallback-entry="agent-list"]')).toBeNull();
      await waitFor(() => expect(frame()?.src).toContain("people.html"));
    });
  }

  it("keeps the source owner alive through the app's StrictMode mount cycle", async () => {
    render(
      <StrictMode>
        <Root />
      </StrictMode>,
    );
    await waitFor(() => expect(frame()?.title).toBe("People"));
    title = "After remount";
    await change();
    await waitFor(() => expect(frame()?.title).toBe("After remount"));
  });
});
