// SPDX-License-Identifier: MIT
/** WebDriver-only observation/launch door. Never writes activity stores. */
import { getActiveTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { plantFixtureOrg, createFixtureForegroundLayout } from "./e2e-cli-workspace-control";
import { emit } from "@tauri-apps/api/event";
import { sessionTarget } from "@/shared/lib/terminal-layout-projection";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import {
  terminalPrepareShellActivity,
  terminalActivitySnapshot,
  writePty,
  TERMINAL_LAYOUT_RESYNC_EVENT,
} from "@/tauri/commands";

const quote = (value: string) => `'${value.replace(/'/g, "'\"'\"'")}'`;
function target(session: string) {
  const result = sessionTarget(useViewerStore.getState().terminalLayout, session);
  if (!result) throw new Error("fixture session is not admitted");
  return result;
}
export function registerActivityE2eControl() {
  window.__CODESENSE_ACTIVITY_E2E__ = {
    showFixture: (path: string) => {
      // Reuse the harness container owner. Do not seed/reset the activity store,
      // layout projection or sessions; the retained host remains authoritative.
      plantFixtureOrg({
        orgSlug: "qh1-activity",
        teamName: "activity",
        teamKey: "qh1-activity:activity",
        sourceRepo: { owner: "fixture", repo: "activity", provider: "github" },
        root: path,
        displayName: "Activity recovery fixture",
        workspaces: [],
        layoutMode: "console",
      });
      useOrgStore.getState().setActiveTeam("activity");
      const teamKey = getActiveTeamKey();
      const viewer = useViewerStore.getState();
      viewer.openTeam(teamKey);
      viewer.openOrFocusGenericWorkspace(teamKey, path);
      const workspace = useViewerStore
        .getState()
        .workspaces[teamKey]?.find((item) => item.kind === "generic");
      if (!workspace) throw new Error("fixture workspace did not open");
      viewer.setLayoutMode("console");
      viewer.setTrayMode(teamKey, "flat");
      useViewerStore.setState((state) => ({
        foregroundLayout: {
          ...state.foregroundLayout,
          [teamKey]: createFixtureForegroundLayout([workspace.id]),
        },
      }));
      viewer.switchWorkspace(teamKey, workspace.id);
      return { teamKey, id: workspace.id };
    },
    diagnostics: () => {
      const org = useOrgStore.getState();
      const viewer = useViewerStore.getState();
      return {
        teamKey: getActiveTeamKey(),
        orgs: org.orgs,
        workspaceRepos: org.workspaceRepos,
        openedTeams: viewer.openedTeams,
        activeWorkspace: viewer.activeWorkspace,
      };
    },
    read: (session: string) => useAgentStateStore.getState().activityBySession[session] ?? null,
    host: (session: string) => {
      const { member, admissionRevision, namespace } = target(session);
      return terminalActivitySnapshot({ member, admissionRevision, namespace });
    },
    resync: () => emit(TERMINAL_LAYOUT_RESYNC_EVENT),
    launch: async (session: string, script: string, marker: string) => {
      const route = target(session);
      const prepared = await terminalPrepareShellActivity(route, "claude");
      await writePty(
        route,
        ` ${quote(prepared.helper)} --activity-exec ${quote(prepared.bootstrap)} -- /bin/bash ${quote(script)} ${quote(prepared.helper)} ${quote(marker)}\r`,
      );
      return prepared.identity;
    },
  };
}
export type ActivityE2eControl = NonNullable<Window["__CODESENSE_ACTIVITY_E2E__"]>;
declare global {
  interface Window {
    __CODESENSE_ACTIVITY_E2E__?: {
      showFixture: (path: string) => { teamKey: string; id: string };
      diagnostics: () => unknown;
      read: (session: string) => import("@/shared/stores/agent-state-store").SessionActivity | null;
      host: (session: string) => ReturnType<typeof terminalActivitySnapshot>;
      resync: () => Promise<void>;
      launch: (
        session: string,
        script: string,
        marker: string,
      ) => Promise<import("@/tauri/commands").ActivityIdentity>;
    };
  }
}
