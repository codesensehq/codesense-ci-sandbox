import "../../../src/index.css";
import { createRoot } from "react-dom/client";
import { SlotDashboardHost } from "../../../src/features/shell/slots/slot-dashboard-host";
import { useDashboardCommandsSync } from "../../../src/features/dashboard/lib/dashboard-commands";
import { dashboardRefKey } from "../../../src/features/dashboard/lib/dashboard-ref";
import { useDashboardProfileStore } from "../../../src/features/dashboard/lib/dashboard-profile";
import { usePaneStore } from "../../../src/features/shell/right-pane/panes/pane-store";
import { INBOX_PANEL_ID } from "../../../src/features/shell/right-pane/right-pane-layout";
import { usePaneOnScreen } from "../../../src/features/shell/right-pane/panes/pane-visibility";
import { useShortcutHandler } from "../../../src/shared/hooks/use-shortcut-handler";
import { useOrgStore } from "../../../src/shared/stores/org-store";
import { useViewerStore } from "../../../src/shared/stores/viewer-store";
import type { DashboardRef, PaneEntry } from "../../../src/tauri/commands";

const api = window as any;
const ref: DashboardRef = { scope: "team", org: "fixture", team: "dev", id: "inbox" };
const entry: PaneEntry = {
  id: "inbox",
  title: "Inbox",
  scope: "team",
  team: "dev",
  path: "/fixture/panes/inbox.html",
  panesDir: "/fixture/panes",
  panelId: "pane:fixture:dev/inbox",
  dashboardRef: ref,
  status: "ready",
  declaration: {
    slots: ["right-pane"],
    capabilities: [],
    commands: [
      {
        id: "archive",
        label: "Archive thread",
        defaultChord: "CmdOrCtrl+Shift+Y",
        scope: "global",
      },
    ],
  },
};
api.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
api.__TAURI_INTERNALS__ = {
  metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
  transformCallback: () => 1,
  unregisterCallback: () => {},
  invoke: async (command: string, args: any) => {
    if (command === "plugin:event|listen") return 1;
    if (command === "get_http_api_port") return api.framePort;
    const grant = {
      frameId: "inbox:1",
      principal: "fixture",
      dashboard: dashboardRefKey(ref),
      slot: "right-pane",
      team: { org: "fixture", team: "dev" },
      scope: "open",
    };
    if (command === "mint_frame")
      return {
        frameId: "inbox:1",
        token: "inbox",
        origin: `http://127.0.0.1:${api.framePort}`,
        kind: "team",
        documentPath: args.args.documentPath,
        grant,
      };
    if (command === "resolve_frame_grant") return grant;
    if (command === "verb_dispatch") return { status: "admit", callId: crypto.randomUUID() };
    if (command === "live_hub_view_key") return "all";
    if (command === "load_app_state") return "{}";
    return null;
  },
};
useOrgStore.setState({ activeOrgSlug: "fixture", activeTeamName: "dev" });
usePaneStore.setState({
  teamKey: "fixture:dev",
  listing: { contentRoot: "/fixture", chain: ["dev"], panes: [entry] },
  knownEntries: {},
});
useDashboardProfileStore.setState({
  org: "fixture",
  profile: {
    id: "default",
    name: "Default",
    posture: "",
    slots: { "right-pane": [ref] },
    cliLayout: "default",
    chrome: {},
  },
});
useViewerStore.setState({ rightPaneOpen: false, activeWorkspace: { "fixture:dev": "terminal" } });
api.viewer = useViewerStore;

function Host() {
  useShortcutHandler();
  useDashboardCommandsSync();
  // Since SV-INP-10 the inbox renders under the host-owned INBOX_PANEL_ID, not the
  // retired per-pane id: reveal targets that panel and command delivery only reaches
  // a frame mounted with it (mirrors PanePanelBody).
  const open = usePaneOnScreen(INBOX_PANEL_ID);
  return (
    <>
      <div className="xterm">
        <textarea id="terminal" aria-label="terminal" />
      </div>
      <div style={{ width: 300, height: 300 }}>
        {open && (
          <SlotDashboardHost
            slotId="right-pane"
            panelId={INBOX_PANEL_ID}
            contentRoot="/fixture"
            visible
            entry={entry}
          />
        )}
      </div>
    </>
  );
}
createRoot(document.getElementById("root")!).render(<Host />);
