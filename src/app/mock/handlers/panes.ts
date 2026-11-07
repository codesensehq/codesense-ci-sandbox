// SPDX-License-Identifier: MIT
import type { HubRow, HubSnapshot } from "@/features/dashboard/lib/live-hub";
import type { HandlerRegistry } from "../mock-router";
import type { MockEventBus } from "../mock-events";
import { PaneSourceProjection } from "../pane-source-projection";
import { MOCK_CONTEXT_ROOT } from "./context-query";
import type {
  DashboardProfile,
  WorkspaceConfig,
  PaneEntry,
  PaneListing,
  FrameRecord,
  MintFrameArgs,
} from "@/tauri/commands";
import {
  PANE_CHANGED_EVENT,
  PANE_OPEN_EVENT,
  type PaneChangedPayload,
  type PaneOpenPayload,
} from "@/tauri/events";

/**
 * Custom right panes in mock mode. The pane files live in the dev server's
 * memory (`scripts/vite-plugin-mock-panes.ts`), which also serves them on the
 * real `/pane/…` route with the real shim, so `list_panes` reads them back over
 * HTTP and a write pushes `pane-changed` through the HMR socket.
 *
 * `window.__codesenseMockPanes` stands in for an agent's MCP tools: `write`
 * (pane_write), `remove` (deleting the file) and `open` (pane_open).
 */

const MOCK_ROOT = MOCK_CONTEXT_ROOT;

interface ServedPane {
  team: string;
  id: string;
  html: string;
  modified: string;
}

function title(html: string, id: string): string {
  const m = /<title[^>]*>([\s\S]*?)<\/title/i.exec(html);
  const text = m?.[1].replace(/\s+/g, " ").trim();
  return text || id;
}

/** A pane header meta value, as the backend reads it. */
function metaContent(html: string, name: string): string | undefined {
  for (const tag of html.match(/<meta\b[^>]*>/gi) ?? []) {
    const attr = (name: string) =>
      new RegExp(`\\s${name}\\s*=\\s*(?:"([^"]*)"|'([^']*)'|([^\\s/>]+))`, "i").exec(tag);
    const n = attr("name");
    if ((n?.[1] ?? n?.[2] ?? n?.[3])?.toLowerCase() !== name) continue;
    const c = attr("content");
    const value = (c?.[1] ?? c?.[2] ?? c?.[3])?.trim();
    return value || undefined;
  }
  return undefined;
}

function panesDir(team: string): string {
  return `${MOCK_ROOT}/${team === "-" ? "private" : team}/panes`;
}

function entry(org: string, p: ServedPane): PaneEntry {
  const priv = p.team === "-";
  const icon = metaContent(p.html, "codesense:icon");
  return {
    id: p.id,
    scope: priv ? "private" : "team",
    ...(priv ? {} : { team: p.team }),
    title: title(p.html, p.id),
    ...(icon ? { icon } : {}),
    surface: metaContent(p.html, "codesense:surface"),
    dashboardRef: {
      scope: priv ? "private" : "team",
      org,
      ...(priv ? {} : { team: p.team }),
      id: p.id,
    },
    declaration: {
      slots: metaContent(p.html, "codesense:slots")?.split(/[\s,]+/) ?? ["right-pane"],
      capabilities: metaContent(p.html, "codesense:capabilities")?.split(/[\s,]+/) ?? [],
      commands: JSON.parse(
        metaContent(p.html, "codesense:commands")?.replace(/&quot;/g, '"') ?? "[]",
      ),
      surface: metaContent(p.html, "codesense:surface"),
    },
    status: "ready",
    path: `${panesDir(p.team)}/${p.id}.html`,
    panesDir: panesDir(p.team),
    panelId: priv ? `pane:private:${org}/${p.id}` : `pane:${org}:${p.team}/${p.id}`,
    modified: p.modified,
  };
}

async function listPanes(org: string, team: string): Promise<PaneListing> {
  const served = (await (await fetch("/__mock-panes")).json()) as ServedPane[];
  const visible = served.filter((p) => p.team === team || p.team === "-");
  visible.sort(
    (a, b) => (a.team === "-" ? 1 : 0) - (b.team === "-" ? 1 : 0) || a.id.localeCompare(b.id),
  );
  return {
    contentRoot: `${MOCK_ROOT}/${team}/content`,
    chain: [team],
    panes: visible.map((p) => entry(org, p)),
  };
}

export interface MockPaneTools {
  write: (team: string | null, id: string, html: string) => Promise<void>;
  remove: (team: string | null, id: string) => Promise<void>;
  open: (panelId: string) => void;
}

export function paneHandlers(
  events: MockEventBus,
  readPrefs: () => Record<string, unknown> = () => ({}),
): HandlerRegistry {
  const sources = new PaneSourceProjection(listPanes, (event, value) => events.emit(event, value));
  const profiles = new Map<string, DashboardProfile>();
  const workspace: WorkspaceConfig = { repos: {} };
  const views = new Map<string, HubSnapshot>();
  const subscriptions = new Map<string, string>();
  const configurationRows = (topic: string, profile?: DashboardProfile): HubRow[] =>
    topic === "workspaceConfig"
      ? Object.entries(workspace.repos).map(([id, value]) => ({ id, value }))
      : Object.entries(profile ?? {}).flatMap<HubRow>(([field, value]) =>
          ["slots", "rightPaneSurfaces", "chrome"].includes(field)
            ? Object.entries(value as Record<string, unknown>).map(([key, value]) => ({
                id: `${field}:${key}`,
                field,
                key,
                value,
              }))
            : [{ id: field, field, value }],
        );
  const publishConfiguration = (topic: string, key: string, rows: HubRow[]) => {
    const before = views.get(key);
    if (before && JSON.stringify(before.snapshot) === JSON.stringify(rows)) return before;
    const view = { topic, viewKey: key, rev: (before?.rev ?? 0) + 1, snapshot: rows };
    views.set(key, view);
    const old = new Map(before?.snapshot.map((row) => [row.id, row]));
    const current = new Set(rows.map((row) => row.id));
    const delta = {
      topic,
      viewKey: key,
      rev: view.rev,
      upserts: rows.filter((row) => JSON.stringify(old.get(row.id)) !== JSON.stringify(row)),
      removes: [...old.keys()].filter((id) => !current.has(id)),
    };
    for (const [subscriptionId, viewKey] of subscriptions)
      if (viewKey === key)
        events.emit("codesense:subscription-delta", {
          subscriptionId,
          baseRev: before?.rev ?? 0,
          delta,
        });
    events.emit("codesense:delta", delta);
    return view;
  };
  // The org the app last listed panes for: the dev server's changes carry none.
  let currentOrg: string | null = null;
  if (typeof window !== "undefined") {
    const tools: MockPaneTools = {
      write: async (team, id, html) => {
        await fetch(`/__mock-panes/${team ?? "-"}/${id}`, { method: "PUT", body: html });
      },
      remove: async (team, id) => {
        await fetch(`/__mock-panes/${team ?? "-"}/${id}`, { method: "DELETE" });
      },
      open: (panelId) => {
        const payload: PaneOpenPayload = { panelId, path: "" };
        events.emit(PANE_OPEN_EVENT, payload);
      },
    };
    (window as unknown as { __codesenseMockPanes: MockPaneTools }).__codesenseMockPanes = tools;
    // The dev server pushes every write; re-emit it as the backend's watcher would.
    import.meta.hot?.on("mock-panes:changed", (change: PaneChangedPayload) => {
      const payload = { ...change, org: currentOrg ?? "" };
      void sources.changed(payload);
      events.emit(PANE_CHANGED_EVENT, payload);
    });
  }
  return {
    configuration_prepare: ({ topic, org, expectedPrincipal, scope }) => {
      if (topic === "prefs") {
        const state = readPrefs();
        const viewer = (state.viewer ?? {}) as Record<string, unknown>;
        const value =
          scope === "global"
            ? { viewer: { settings: viewer.settings ?? {} }, shortcuts: state.shortcuts ?? {} }
            : {
                dashboardPolicy: state.dashboardPolicy ?? {},
                viewer: Object.fromEntries(
                  Object.entries(viewer).filter(([key]) => key !== "settings"),
                ),
              };
        const rows: HubRow[] = [];
        const flatten = (value: unknown, path: string[]) => {
          if (
            value &&
            typeof value === "object" &&
            !Array.isArray(value) &&
            Object.keys(value).length
          )
            for (const [key, field] of Object.entries(value)) flatten(field, [...path, key]);
          else rows.push({ id: JSON.stringify(path), path, value });
        };
        flatten(value, []);
        return publishConfiguration(
          "prefs",
          scope === "global" ? "prefs:global" : `prefs:${expectedPrincipal}:native`,
          rows,
        );
      }
      const name = String(topic);
      const key =
        name === "profile"
          ? `profile:${expectedPrincipal}:${org}`
          : `workspaceConfig:${expectedPrincipal}:native`;
      return publishConfiguration(name, key, configurationRows(name, profiles.get(String(org))));
    },
    write_dashboard_profile: ({ org, patch }) => {
      const previous = profiles.get(String(org)) ?? {
        id: "default",
        name: "Default",
        posture: "",
        cliLayout: "default",
        slots: {},
        chrome: {},
      };
      const fields = patch as Partial<DashboardProfile>;
      const profile = {
        ...previous,
        ...fields,
        slots: { ...previous.slots, ...fields.slots },
        rightPaneSurfaces: { ...previous.rightPaneSurfaces, ...fields.rightPaneSurfaces },
        chrome: { ...previous.chrome, ...fields.chrome },
      };
      profiles.set(String(org), profile);
      for (const [key, view] of views)
        if (view.topic === "profile" && key.endsWith(`:${org}`))
          publishConfiguration("profile", key, configurationRows("profile", profile));
      return profile;
    },
    load_workspace_config: () => workspace,
    save_workspace_config: ({ config }) => {
      for (const [id, fields] of Object.entries((config as WorkspaceConfig).repos))
        workspace.repos[id] = { ...workspace.repos[id], ...fields };
      for (const [key, view] of views)
        if (view.topic === "workspaceConfig")
          publishConfiguration("workspaceConfig", key, configurationRows("workspaceConfig"));
    },
    commands_sync: () => null,
    list_panes: (args) => {
      currentOrg = String(args.org);
      return sources.read(args);
    },
    live_hub_subscribe: (args) => {
      if (args.topic === "profile" || args.topic === "workspaceConfig" || args.topic === "prefs") {
        if (args.subscriptionId)
          subscriptions.set(String(args.subscriptionId), String(args.viewKey));
        return views.get(String(args.viewKey));
      }
      return sources.snapshot(String(args.viewKey));
    },
    live_hub_unsubscribe: (args) => {
      subscriptions.delete(String(args.subscriptionId));
      sources.release(String(args.subscriptionId));
    },
    // The dev server is the mock's HTTP API: panes load from it on `/pane/…`.
    get_http_api_port: () => Number(window.location.port) || 80,
    mint_frame: async ({ args }) => {
      const response = await fetch("/__mock-frames", {
        method: "POST",
        body: JSON.stringify(args as unknown as MintFrameArgs),
      });
      return response.json() as Promise<FrameRecord>;
    },
    rebind_frame_team: async ({ frameId, token, org, team }) => {
      const response = await fetch(`/__mock-frames/${frameId}/rebind-team`, {
        method: "POST",
        body: JSON.stringify({ token, org, team }),
      });
      if (!response.ok) throw new Error("denied: frame identity or slot");
      return response.json() as Promise<FrameRecord>;
    },
    release_frame: async ({ frameId }) => {
      await fetch(`/__mock-frames/${frameId}`, { method: "DELETE" });
    },
    resolve_frame_grant: async ({ origin, token }) => {
      const response = await fetch("/__mock-frames/resolve", {
        method: "POST",
        body: JSON.stringify({ origin, token }),
      });
      if (!response.ok) throw new Error("denied: frame identity");
      return response.json();
    },
  };
}
