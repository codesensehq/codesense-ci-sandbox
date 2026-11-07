import "../../../src/index.css";
import { createRoot } from "react-dom/client";
import { TooltipProvider } from "../../../src/components/ui/tooltip";
import { HostMenuLayer } from "../../../src/features/shell/host-menu-layer";
import { VerbConfirmationProvider } from "../../../src/features/dashboard/components/verb-confirmation-provider";
import { EndConfirmDialog } from "../../../src/features/viewer/components/end-confirm-dialog";
import { useAgentListProjectionStore } from "../../../src/features/viewer/lib/agent-list-projection";
import { liveHub, type HubDelta } from "../../../src/features/dashboard/lib/live-hub";
import { PaneSourceProjection } from "../../../src/app/mock/pane-source-projection";
import { terminalManager } from "../../../src/shared/terminal/terminal-manager";
import { WorkspaceTray } from "../../../src/features/viewer/components/workspace-tray";
import { DockSlotHost } from "../../../src/features/shell/slots/dock-slot-host";
import { useDashboardProfileStore } from "../../../src/features/dashboard/lib/dashboard-profile";
import { PanePanelsMount } from "../../../src/features/shell/right-pane/panes/use-register-pane-panels";
import type { PaneEntry } from "../../../src/tauri/commands";
import { useViewerStore } from "../../../src/shared/stores/viewer-store";
import { useOrgStore } from "../../../src/shared/stores/org-store";
import {
  requestFrameFocus,
  retireAfterFlush,
} from "../../../src/features/dashboard/lib/frame-lifecycle";
import { DOCK_FRAME_PANEL_ID } from "../../../src/features/shell/slots/slot-registry";
import {
  runDashboardCommand,
  useDashboardCommandsSync,
} from "../../../src/features/dashboard/lib/dashboard-commands";
import { dashboardRefKey } from "../../../src/features/dashboard/lib/dashboard-ref";
import { dashboardCommandActionId } from "../../../src/shared/lib/shortcuts/registry";

// The dock host with a chosen fixture dashboard. `?page=` picks the fixture (dock-ok, dock-never,
// dock-throw); `?slow` delays the agentList subscription so first paint is visible on its own.
const api = window as any;
const params = new URLSearchParams(location.search);
const pageName = params.get("page") ?? "dock-ok";
const fixtureWorkspace = {
  id: "terminal",
  kind: "generic" as const,
  status: "ready" as const,
  cwd: "/fixture/workspace",
};
api.calls = [];
api.rebinds = [];
const revoked = new Set<string>();
const grants = new Map<string, Record<string, unknown>>();
const session = {
  id: "fixture-session",
  sessionId: "fixture-session",
  handle: "fixture-agent",
  kind: "cli",
  alive: true,
  backgrounded: false,
  startedWith: null,
  startedAtMs: null,
  launcher: null,
  launchTeamKey: "fixture:dev",
  team: { org: "fixture", team: "dev" },
  rowNames: { primary: "Fixture", secondary: null, full: "Fixture" },
};
const reference = {
  id: "fixture-agent",
  sessionRef: "fixture-session",
  inboxRef: "fixture-agent",
  teamKey: "fixture:dev",
  parentId: null,
  parentSource: null,
  order: 0,
  workspaceRef: pageName === "dock-agent-list" ? fixtureWorkspace.id : null,
  workspace:
    pageName === "dock-agent-list"
      ? { id: fixtureWorkspace.id, label: "Fixture workspace", kind: "folder" as const }
      : null,
  storyRef: null,
  workspaceRank: 0,
};
const agentState = {
  id: "agentList:state",
  rowType: "state",
  ready: true,
  order: Number.MAX_VALUE,
  teamOrder: ["fixture:dev"],
  teamNames: { "fixture:dev": { org: "Fixture", team: "dev" } },
  pane: { workspaceOrder: { "fixture:dev": [] } },
};
const uiDock = { id: "ui:dock", arrange: "teams", folds: {}, pins: {} };
terminalManager.holdsVisible = () => true;
terminalManager.watchShown = () => () => {};
let rebound = 0;
let minted = 0;
const framePaths = new Map<string, string>();
api.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
api.__TAURI_INTERNALS__ = {
  metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
  transformCallback: () => 1,
  unregisterCallback: () => {},
  invoke: async (command: string, args: any) => {
    api.calls.push([command, args]);
    if (command === "plugin:event|listen") return 1;
    if (command === "get_http_api_port") return api.framePort;
    if (command === "mint_frame") {
      const id = args.args.documentPath.split("/").pop();
      const frameId = `${id}:${++minted}`;
      framePaths.set(frameId, args.args.documentPath);
      const record = {
        frameId,
        token: id,
        origin: `http://127.0.0.1:${api.framePort}`,
        kind: "team",
        documentPath: args.args.documentPath,
        policy: {},
        grant: {
          frameId,
          principal: "fixture",
          dashboard: dashboardRefKey(ref),
          slot: args.args.slot,
          team: { org: "fixture", team: "dev" },
          scope: "open",
        },
      };
      grants.set(record.token, record.grant);
      return record;
    }
    if (command === "list_panes") {
      api.listed = (api.listed ?? 0) + 1;
      return paneSources.read(args);
    }
    if (command === "read_dashboard_profile") return useDashboardProfileStore.getState().profile;
    if (command === "rebind_frame_team") {
      // `?slow-rebind` holds the first re-mint so a second switch lands while it is pending.
      if (params.has("slow-rebind") && rebound === 0)
        await new Promise((done) => setTimeout(done, 400));
      // As Rust does: refuse a stale token, mint a new one, revoke the old one at once.
      if (!grants.has(args.token) || revoked.has(args.token))
        throw new Error("denied: frame identity");
      const grant = { ...grants.get(args.token)!, team: { org: args.org, team: args.team } };
      const token = `${args.frameId}#${++rebound}`;
      revoked.add(args.token);
      grants.delete(args.token);
      grants.set(token, grant);
      api.rebinds.push({ from: args.token, to: token, team: args.team });
      return {
        frameId: args.frameId,
        token,
        origin: `http://127.0.0.1:${api.framePort}`,
        kind: "team",
        documentPath: framePaths.get(args.frameId)!,
        contentRoot: "/fixture",
        policy: {},
        grant,
      };
    }
    if (command === "resolve_frame_grant") {
      if (revoked.has(args.token) || !grants.has(args.token))
        throw new Error("denied: frame identity");
      return grants.get(args.token);
    }
    if (command === "verb_dispatch") {
      return { status: "admit", callId: crypto.randomUUID() };
    }
    if (command === "verb_settle") return null;
    if (command === "agent_read_roles") return { roles: [] };
    if (command === "avatar_packs_list_for_team")
      return { packs: [], warnings: [], default_pack: null };
    if (command === "live_hub_subscribe") {
      if (args.topic === "paneSources") return paneSources.snapshot(args.viewKey);
      if (args.topic === "agentList" && params.has("slow"))
        await new Promise((done) => setTimeout(done, 1500));
      return {
        topic: args.topic,
        viewKey: args.viewKey,
        rev: 1,
        snapshot:
          args.topic === "sessions"
            ? [session]
            : args.topic === "agentList"
              ? [reference, agentState]
              : args.topic === "ui:dock"
                ? [uiDock]
                : [],
      };
    }
    if (command === "live_hub_unsubscribe") {
      paneSources.release(args.subscriptionId);
      return null;
    }
    if (command === "local_profile_status") return { active: { principal: "fixture" } };
    if (command === "live_hub_view_key") return "all";
    if (command === "bridge_doc_context")
      return { org: "fixture", team: "dev", layer: "team", contentRoot: "/fixture" };
    if (command === "query_context") return { rows: [], total: 0 };
    if (command === "load_app_state") return "{}";
    if (command === "agent_read_sessions") return [session];
    return null;
  },
};
// `?team=ops` starts on a sibling team whose listing does not hold the chosen dashboard.
const startTeam = params.get("team") ?? "dev";
useOrgStore.setState({ activeOrgSlug: "fixture", activeTeamName: startTeam });
useViewerStore.setState({
  activeWorkspace: { "fixture:dev": "terminal", "fixture:ops": "terminal" },
  terminalVisible: true,
  workspacePanelCollapsed: false,
  workspacePanelWidth: 236,
  ...(pageName === "dock-agent-list"
    ? { workspaces: { "fixture:dev": [fixtureWorkspace] }, workspaceTraySeenCoachmark: true }
    : {}),
});
useAgentListProjectionStore.setState({ sessions: [session] as never, references: [reference] });
liveHub.seed({ topic: "sessions", viewKey: "all", rev: 1, snapshot: [session] });
if (pageName === "dock-agent-list")
  liveHub.seed({ topic: "ui:dock", viewKey: "all", rev: 1, snapshot: [uiDock] });
const ref = { scope: "team" as const, org: "fixture", team: "dev", id: pageName };
useDashboardProfileStore.setState({
  org: "fixture",
  profile: {
    id: "default",
    name: "Default",
    posture: "",
    slots: { dock: ref },
    cliLayout: "default",
    chrome: {},
  },
});
const entry: PaneEntry = {
  id: pageName,
  title: "Fixture agents",
  scope: "team" as const,
  team: "dev",
  path: `/fixture/${pageName}.html`,
  panesDir: "/fixture",
  panelId: `pane:fixture:dev/${pageName}`,
  dashboardRef: ref,
  declaration: {
    slots: ["dock"],
    capabilities: [],
    commands: [{ id: "inspect", label: "Inspect agents", scope: "global" }],
  },
};
// `?builtin` lists the built-in agent-list page (as Rust `panes/builtin.rs` always does), the
// dock's HTML fallback when the chosen dashboard fails. Without it the dock has no page to fall
// back to and shows its native list.
const builtinAgentList: PaneEntry = {
  id: "agent-list",
  title: "Agents",
  scope: "team" as const,
  team: "dev",
  path: "/fixture/agent-list.html",
  panesDir: "/fixture",
  panelId: "pane:built-in/agent-list",
  dashboardRef: { scope: "built-in" as const, org: "fixture", id: "agent-list" },
  declaration: { slots: ["dock"], capabilities: [], commands: [] },
};
const listedBuiltin = params.has("builtin") ? [builtinAgentList] : [];
const paneSources = new PaneSourceProjection(
  (_org, team) =>
    team === "dev"
      ? {
          contentRoot: "/fixture",
          chain: ["dev"],
          panes: params.has("missing") && !api.restored ? listedBuiltin : [entry, ...listedBuiltin],
        }
      : { contentRoot: "/fixture-ops", chain: [team], panes: [] },
  (event, payload) => {
    if (event === "codesense:delta") liveHub.receive(payload as HubDelta);
  },
);
api.removeAgentRow = () => {
  liveHub.receive({
    topic: "agentList",
    viewKey: "all",
    rev: 2,
    upserts: [],
    removes: [reference.id],
  });
};
// Lifecycle seams the tests drive: focus requests, right-pane retirement, file-change reloads.
api.focusDock = () => requestFrameFocus(DOCK_FRAME_PANEL_ID);
api.retireRightPane = (panelId: string) =>
  retireAfterFlush(
    (f) => f.panelId === panelId,
    () => {},
  );
api.bumpReload = () =>
  paneSources.changed({ scope: "team", org: "fixture", team: "dev", id: pageName });
api.entryPanelId = entry.panelId;
// The fixture page reports its own save and teardown, in order with the host's calls.
addEventListener("message", (e) => {
  if (e.data?.fixture) api.calls.push([`fixture:${e.data.fixture}`]);
});
// A Settings choice: the fixture dashboard or the built-in list.
api.choose = (choice: "dashboard" | "native") =>
  useDashboardProfileStore.setState((state) => ({
    profile: { ...state.profile!, slots: { dock: choice === "native" ? "native" : ref } },
  }));
api.restore = () => {
  api.restored = true;
};
// A team switch, as the org store sees one.
api.switchTeam = (team: string) => {
  useOrgStore.setState({ activeTeamName: team });
};
api.foldDock = () => useViewerStore.setState({ workspacePanelCollapsed: true });
api.runDockCommand = (args: unknown) =>
  runDashboardCommand(dashboardCommandActionId(dashboardRefKey(ref), "inspect"), args);

function Host() {
  useDashboardCommandsSync();
  const team = useOrgStore((s) => s.activeTeamName);
  return (
    <>
      <div style={{ display: "flex", height: 420 }}>
        <DockSlotHost teamKey={`fixture:${team}`} visible />
        <div data-xterm-host style={{ flex: 1 }}>
          <div data-pane-id="terminal">
            <div className="xterm">
              <textarea id="terminal" aria-label="terminal" />
            </div>
          </div>
        </div>
      </div>
      {pageName === "dock-agent-list" && (
        <WorkspaceTray
          teamKey={`fixture:${team}`}
          tree={null}
          isLightTheme={false}
          onNewWorkspace={() => {}}
        />
      )}
    </>
  );
}
createRoot(document.getElementById("root")!).render(
  <TooltipProvider>
    <VerbConfirmationProvider>
      <HostMenuLayer>
        <PanePanelsMount />
        <Host />
        <EndConfirmDialog />
      </HostMenuLayer>
    </VerbConfirmationProvider>
  </TooltipProvider>,
);
