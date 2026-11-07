// SPDX-License-Identifier: MIT
import { useAgentListFacts } from "./agent-list-facts";
import { agentListMirrorInputs } from "./agent-list-mirror";
import { listen } from "@tauri-apps/api/event";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { agentListIdentity, agentListSetIdentity, pushViewerState } from "@/tauri/commands";
import {
  useViewerStore,
  selectFocusedSession,
  viewerMirrorFields,
  type ViewerState,
} from "@/shared/stores/viewer-store";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import type { AgentIdentityChoice } from "@/shared/lib/agent-identity-choice";

import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { agentSessionModel } from "./agent-session-model";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";

/** Every full viewer-state push replaces the native snapshot, so each one must carry all of these. */
export function agentListPushFields(state: ViewerState) {
  const mirrored = agentListMirrorInputs(state, useOrgStore.getState().orgs);
  return {
    ...mirrored,
    agentSessionModels: sessionModels(),
    agentSessionFacts: useAgentListFacts.getState().sessions,
    agentPane: {
      ...mirrored.agentPane,
      ...useAgentListFacts.getState().pane,
    },
    agentStageStates: useStatusTaxonomyStore.getState().states,
    agentPendingAsks: pendingAskTexts(),
    orgDisplayNames: Object.fromEntries(
      useOrgStore.getState().orgs.map((org) => [org.slug, org.display_name]),
    ),
  };
}

type Identities = Record<string, Record<string, AgentIdentityChoice>>;

/** Workspace choices remain a viewer mirror; only these actions flush it immediately. */
export function watchAgentListInputs(): () => void {
  let disposed = false;
  let applying = false;
  let initialized = false;
  let queue = Promise.resolve();
  const report = (error: unknown) => console.error("Agent-list input update failed", error);
  // The dock's arrangement, pins and per-node folds (`ui:dock`, one view key for every team): the
  // built-in list and a dock dashboard read and write the same state through the hub.
  const dockState = () => {
    const state = useViewerStore.getState();
    return {
      arrange: state.agentsPaneArrange,
      pins: state.agentPins,
      folds: state.agentsPaneFolds,
      hiddenTeams: state.agentsPaneHiddenTeams ?? [],
      search: state.agentsPaneSearch ?? { query: "", status: "all" },
      workspaceFilter: state.agentsWorkspaceFilter ?? null,
      agentFilter: state.agentsListFilter ?? null,
      teamFocus: state.agentsPaneTeamFocusRestore ?? null,
    };
  };
  const applyDock = (dock: Record<string, unknown>) => {
    applying = true;
    useViewerStore.setState({
      ...("hiddenTeams" in dock ? { agentsPaneHiddenTeams: dock.hiddenTeams as string[] } : {}),
      ...("search" in dock
        ? { agentsPaneSearch: dock.search as ReturnType<typeof dockState>["search"] }
        : {}),
      ...("workspaceFilter" in dock
        ? {
            agentsWorkspaceFilter: dock.workspaceFilter as ReturnType<
              typeof dockState
            >["workspaceFilter"],
          }
        : {}),
      ...("agentFilter" in dock
        ? { agentsListFilter: dock.agentFilter as ReturnType<typeof dockState>["agentFilter"] }
        : {}),
      ...("teamFocus" in dock
        ? {
            agentsPaneTeamFocusRestore: dock.teamFocus as ReturnType<typeof dockState>["teamFocus"],
          }
        : {}),
      agentsPaneArrange: dock.arrange as "teams" | "status" | "manager",
      agentPins: dock.pins as ReturnType<typeof useViewerStore.getState>["agentPins"],
      ...(isFolds(dock.folds) ? { agentsPaneFolds: dock.folds } : {}),
    });
    applying = false;
  };
  const flushMirror = () => {
    const state = useViewerStore.getState();
    if (!state.stateLoaded) return;
    // A push replaces the whole mirror: it carries the viewer's fields, not only the agent list's.
    void pushViewerState({ ...viewerMirrorFields(state), ...agentListPushFields(state) }).catch(
      report,
    );
  };
  const initialize = () => {
    const state = useViewerStore.getState();
    if (initialized || !state.stateLoaded) return;
    initialized = true;
    flushMirror();
    queue = agentListIdentity(state.agentIdentity)
      .then((identity) => {
        if (disposed) return;
        applying = true;
        useViewerStore.setState({ agentIdentity: identity });
        applying = false;
      })
      .catch(report);
    void liveHub
      .snapshot("ui:dock", "all")
      .then((snapshot) => {
        if (disposed) return;
        const dock = snapshot.snapshot[0];
        if (dock?.arrange && dock?.pins) applyDock(dock);
        if (
          !dock?.arrange ||
          !dock?.pins ||
          !isFolds(dock.folds) ||
          !dock.search ||
          !dock.hiddenTeams
        )
          void liveHub.writeUi("ui:dock", "all", dockState()).catch(report);
      })
      .catch(report);
  };
  const stop = useViewerStore.subscribe((state, previous) => {
    if (!state.stateLoaded) {
      initialized = false;
      return;
    }
    const restoring = !initialized;
    initialize();
    if (restoring || !initialized || applying) return;
    if (
      state.workspaces !== previous.workspaces ||
      state.softClosedWorkspaces !== previous.softClosedWorkspaces ||
      state.openedTeams !== previous.openedTeams ||
      state.workspaceOrderByTeam !== previous.workspaceOrderByTeam ||
      state.trayBaseOrderByTeam !== previous.trayBaseOrderByTeam ||
      state.railKeyByWorkspaceId !== previous.railKeyByWorkspaceId ||
      state.customGroupsByTeam !== previous.customGroupsByTeam
    )
      flushMirror();
    if (state.agentIdentity !== previous.agentIdentity) {
      for (const team of new Set([
        ...Object.keys(state.agentIdentity),
        ...Object.keys(previous.agentIdentity),
      ])) {
        for (const handle of new Set([
          ...Object.keys(state.agentIdentity[team] ?? {}),
          ...Object.keys(previous.agentIdentity[team] ?? {}),
        ])) {
          const choice = state.agentIdentity[team]?.[handle] ?? null;
          if (
            JSON.stringify(choice) ===
            JSON.stringify(previous.agentIdentity[team]?.[handle] ?? null)
          )
            continue;
          queue = queue
            .then(() => agentListSetIdentity(team, handle, choice))
            .then(() => {})
            .catch(report);
        }
      }
    }
    if (
      state.agentsPaneArrange !== previous.agentsPaneArrange ||
      state.agentPins !== previous.agentPins ||
      state.agentsPaneFolds !== previous.agentsPaneFolds ||
      state.agentsPaneHiddenTeams !== previous.agentsPaneHiddenTeams ||
      state.agentsPaneSearch !== previous.agentsPaneSearch ||
      state.agentsWorkspaceFilter !== previous.agentsWorkspaceFilter ||
      state.agentsListFilter !== previous.agentsListFilter ||
      state.agentsPaneTeamFocusRestore !== previous.agentsPaneTeamFocusRestore
    ) {
      void liveHub.writeUi("ui:dock", "all", dockState()).catch(report);
    }
  });
  // The selected row is the CLI's focused session; the host mirrors it into `ui:dock.selected`
  // and a dashboard changes it with gotoAgent, never by writing it.
  let selected: string | null | undefined;
  const mirrorSelected = () => {
    const { activeOrgSlug: org, activeTeamName: team } = useOrgStore.getState();
    const state = useViewerStore.getState();
    if (!state.stateLoaded) return;
    const next = (org && team && selectFocusedSession(state, buildTeamKey(org, team))) || null;
    if (next === selected) return;
    selected = next;
    void liveHub.writeUi("ui:dock", "all", { selected: next }).catch(report);
  };
  const stopModels = useAgentStateStore.subscribe((state, previous) => {
    if (
      state.launchBySession !== previous.launchBySession ||
      state.bySession !== previous.bySession
    ) {
      if (JSON.stringify(sessionModels()) !== JSON.stringify(sessionModels(previous)))
        flushMirror();
    }
  });
  const stopFacts = useAgentListFacts.subscribe(flushMirror);
  const stopStages = useStatusTaxonomyStore.subscribe((state, previous) => {
    if (state.states !== previous.states) flushMirror();
  });
  const stopAsks = useManagedSessionStore.subscribe((state, previous) => {
    if (state.pendingAsks !== previous.pendingAsks) flushMirror();
  });
  const stopSelected = useViewerStore.subscribe(mirrorSelected);
  const stopOrgs = useOrgStore.subscribe((state, previous) => {
    if (state.orgs !== previous.orgs) flushMirror();
    mirrorSelected();
  });
  const identityListener = listen<Identities>("agent-list-identity-changed", ({ payload }) => {
    if (disposed) return;
    applying = true;
    useViewerStore.setState({ agentIdentity: payload });
    applying = false;
  });
  const stopDock = liveHub.watch("ui:dock", "all", (snapshot) => {
    const dock = snapshot.snapshot[0];
    if (!initialized || !dock?.arrange || !dock?.pins) return;
    applyDock(dock);
  });
  initialize();
  return () => {
    disposed = true;
    stop();
    stopOrgs();
    stopSelected();
    stopModels();
    stopAsks();
    stopStages();
    stopFacts();
    stopDock();
    void identityListener.then((unlisten) => unlisten());
  };
}

function isFolds(value: unknown): value is Record<string, boolean> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function sessionModels(state = useAgentStateStore.getState()) {
  return Object.fromEntries(
    [...new Set([...Object.keys(state.launchBySession), ...Object.keys(state.bySession)])].map(
      (sid) => [sid, agentSessionModel(state, sid)],
    ),
  );
}

function pendingAskTexts() {
  const texts: Record<string, string> = {};
  for (const ask of Object.values(useManagedSessionStore.getState().pendingAsks)) {
    texts[ask.sessionId] ??= ask.question;
  }
  return texts;
}
