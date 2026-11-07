// SPDX-License-Identifier: MIT
// SW-GW-2: the tab-management verbs — the DRIVE half of a dashboard-opened
// generic (dir) workspace. `openTab` adds an addressable tab to an already-open
// generic workspace (optionally hosting an agent); `sendToWorkspaceTab` delivers
// a shell `command` or an agent `prompt` to a tab addressed by its `sessionId`.
//
// These are handle-addressed, NOT doc-scoped: they operate on the global viewer
// store by `workspaceId`/`sessionId`, so — unlike the SW-DX-7 spawn verbs — they
// do NOT need the render-scoped dashboard-launch controller. The html-bridge
// handler still fails closed when no dashboard is mounted (the controller-null
// guard); the boundary enforced HERE is that a tab is addressable ONLY when it
// belongs to an OPEN `generic` workspace — so these tab verbs never reach a
// story/branch/epic agent session (those go through the story verbs).

import type { WorkspaceKind } from "@/shared/lib/workspace-kind-capabilities";
import { selectSessionHome, useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { getActiveTeamKey } from "@/shared/lib/team-key";
import { injectAgentLine, type GateMode } from "@/shared/lib/agent-session-injection";
import {
  agentBasename,
  agentLaunchLine,
  isAgentConnected,
  recordAgentLaunch,
} from "@/shared/lib/agent-session-launcher";
import { projectMcpScopeConfig } from "@/tauri/commands";
import { DEFAULT_AGENT_CLI, resolveAgentCliPath } from "@/shared/lib/default-agent";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";

/** The role tag on a generic-workspace tab that hosts an agent (SW-GW-2). A
 *  free string tag on the `TerminalTab` — mirrors other role tags like
 *  `NOTES_ROLE`. */
export const GENERIC_AGENT_ROLE = "generic-agent";

// ── openTab ──────────────────────────────────────────────────────────────────

export interface OpenWorkspaceTabDeps {
  /** The kind of an OPEN workspace by id (wherever it is open), or `null` when no
   *  such workspace is open — the existence + kind gate. */
  workspaceKindOf: (workspaceId: string) => WorkspaceKind | null;
  /** Mint + add a tab to the workspace and return its `sessionId` (`""` if the
   *  workspace vanished). `role` tags an agent tab. */
  openTab: (workspaceId: string, role?: string) => string;
  /** Remove a just-opened tab — the rollback for a failed agent launch (so a
   *  thrown launch never leaves an orphan, mislabeled agent tab behind). */
  closeTab: (workspaceId: string, sessionId: string) => void;
  /** Launch the agent CLI into the tab's shell (SW-GW-2 agent tab), with its
   *  per-session config for the workspace's dir. Rejects `unavailable:` when the
   *  agent CLI can't be resolved. */
  launchAgent: (workspaceId: string, sessionId: string) => Promise<void>;
}

/**
 * `openTab({workspaceId, agent?}) → {sessionId}` — add a tab to an already-open
 * GENERIC (dir) workspace and return its addressable `sessionId`. `agent:true`
 * additionally launches the agent CLI into the new tab so it can host `prompt`s.
 * Refuses any non-generic / un-opened `workspaceId` (AC-3/AC-7).
 */
export async function openWorkspaceTab(
  args: { workspaceId?: unknown; agent?: unknown },
  deps: OpenWorkspaceTabDeps,
): Promise<{ sessionId: string }> {
  const workspaceId = typeof args.workspaceId === "string" ? args.workspaceId : "";
  if (!workspaceId) {
    throw new Error("openTab: { workspaceId } must be a non-empty string");
  }
  const kind = deps.workspaceKindOf(workspaceId);
  if (kind === null) {
    throw new Error(`unavailable: no open workspace "${workspaceId}"`);
  }
  if (kind !== "generic") {
    throw new Error(`not-allowed: openTab targets generic (dir) workspaces only, not "${kind}"`);
  }
  const wantsAgent = args.agent === true;
  const sessionId = deps.openTab(workspaceId, wantsAgent ? GENERIC_AGENT_ROLE : undefined);
  if (!sessionId) {
    throw new Error(`unavailable: could not open a tab in "${workspaceId}"`);
  }
  if (wantsAgent) {
    try {
      await deps.launchAgent(workspaceId, sessionId);
    } catch (err) {
      // The tab was minted + role-tagged BEFORE the launch. A failed launch (e.g.
      // no agent CLI) would otherwise strand a `generic-agent`-tagged tab with no
      // agent behind it — roll it back so state matches the rejected result.
      deps.closeTab(workspaceId, sessionId);
      throw err;
    }
  }
  return { sessionId };
}

// ── sendToWorkspaceTab ───────────────────────────────────────────────────────

/** What a `sessionId` resolves to when it belongs to an open workspace tab. */
export interface WorkspaceTabInfo {
  kind: WorkspaceKind;
  /** The tab was opened as an agent tab (role === GENERIC_AGENT_ROLE) — INTENT,
   *  not liveness; liveness is a separate async probe (`agentLive`). */
  isAgentTab: boolean;
}

export interface SendToWorkspaceTabDeps {
  /** Resolve `sessionId` → its open-workspace tab info, or `null` when it is not
   *  a tab of any OPEN workspace (the trust boundary — an unknown/foreign session
   *  is never addressable). */
  tabInfo: (sessionId: string) => WorkspaceTabInfo | null;
  /** AC-4 (`command`): submit shell text to the tab; `{submitted}` reflects the
   *  bounded submit outcome. */
  sendCommand: (sessionId: string, text: string) => Promise<{ submitted: boolean }>;
  /** True `<agent>`-process liveness in the tab (connected vs dropped to a bare
   *  shell) — the guard that stops a `prompt` landing in a dead-agent shell. */
  agentLive: (sessionId: string) => Promise<boolean>;
  /** AC-4 (`prompt`): inject+submit prompt text to the CONFIRMED-LIVE agent. */
  sendPrompt: (sessionId: string, text: string) => Promise<{ submitted: boolean }>;
}

/**
 * `sendToWorkspaceTab({sessionId, mode, text}) → {submitted}` — deliver text to a
 * specific tab. `command` submits to the shell; `prompt` injects into a LIVE agent
 * (refusing `no-agent-in-tab` if the tab was never opened as an agent, or its
 * agent is not currently live — a prompt is not sent to a tab with no live agent,
 * best-effort against a small probe→submit race, AC-5).
 * Addressable ONLY for a tab of an open GENERIC workspace (AC-7 trust boundary).
 */
export async function sendToWorkspaceTab(
  args: { sessionId?: unknown; mode?: unknown; text?: unknown },
  deps: SendToWorkspaceTabDeps,
): Promise<{ submitted: boolean }> {
  const sessionId = typeof args.sessionId === "string" ? args.sessionId : "";
  const text = typeof args.text === "string" ? args.text : "";
  const mode = args.mode;
  if (!sessionId) {
    throw new Error("sendToWorkspaceTab: { sessionId } must be a non-empty string");
  }
  if (mode !== "command" && mode !== "prompt") {
    throw new Error("sendToWorkspaceTab: { mode } must be 'command' or 'prompt'");
  }
  if (!text) {
    throw new Error("sendToWorkspaceTab: { text } must be a non-empty string");
  }
  const info = deps.tabInfo(sessionId);
  if (!info) {
    // Trust boundary: only a tab of an OPEN workspace is addressable.
    throw new Error(`unavailable: no open workspace tab for session "${sessionId}"`);
  }
  if (info.kind !== "generic") {
    // Never let this verb touch a story/branch/epic agent tab (AC-7).
    throw new Error("not-allowed: sendToWorkspaceTab targets generic (dir) workspace tabs only");
  }
  if (mode === "prompt") {
    // A prompt only makes sense against a live agent. Role (intent) AND a real
    // liveness probe must BOTH pass — a tab that was never opened as an agent, or
    // whose agent has crashed back to a shell, is refused (AC-5). This is
    // best-effort: a tiny TOCTOU window remains between the probe and the submit
    // (the agent could exit in between), so the guarantee is "checked live," not
    // atomic — acceptable for v1 (the common dead-agent tab IS caught).
    if (!info.isAgentTab || !(await deps.agentLive(sessionId))) {
      throw new Error("no-agent-in-tab");
    }
    return deps.sendPrompt(sessionId, text);
  }
  return deps.sendCommand(sessionId, text);
}

// ── real deps (store + PTY primitives) ───────────────────────────────────────

/** Resolve an OPEN workspace by id to its owning team, PREFERRING the active team
 *  and falling back to a global scan. A generic `workspaceId` (`generic:<path>`)
 *  is NOT team-namespaced, so two teams that opened the same dir share one id —
 *  the active-team preference disambiguates to the operator's current team (the
 *  one a dashboard's own `openWorkspace(dir)` opened into), while the global
 *  fallback still reaches a workspace open only in another team. */
function resolveWorkspaceTeam(
  workspacesByTeam: Record<string, Workspace[]>,
  workspaceId: string,
): { teamKey: string; workspace: Workspace } | null {
  const activeTeam = getActiveTeamKey();
  const inActive = (workspacesByTeam[activeTeam] ?? []).find((w) => w.id === workspaceId);
  if (inActive) return { teamKey: activeTeam, workspace: inActive };
  for (const [teamKey, list] of Object.entries(workspacesByTeam)) {
    const workspace = list.find((w) => w.id === workspaceId);
    if (workspace) return { teamKey, workspace };
  }
  return null;
}

/** Submit a line to a tab via the guarded injector and map the outcome to
 *  `{submitted}` — the single submit path shared by the launch line, `command`,
 *  and `prompt` (they differ ONLY by the readiness `gate`). */
function submitLine(
  sessionId: string,
  text: string,
  gate: GateMode,
): Promise<{ submitted: boolean }> {
  return injectAgentLine(sessionId, text, undefined, { gate, submit: true }).then((o) => ({
    submitted: o.ok,
  }));
}

/** Find a session's tab AND its owning workspace kind across all open teams. The
 *  session id is an unguessable uuid the caller obtained from its own granted
 *  `openTab`, so a global lookup is safe; membership in an OPEN workspace is the
 *  authorization (AC-7). Finds it shown, Hidden, or in a tile hidden in the
 *  tray: each keeps a live PTY, so it stays addressable. A send reaches a hidden
 *  tab where it is — the user hid it on purpose, so a dashboard write never yanks
 *  it back onto the tile (the same rule as a tab whose tile is hidden in the
 *  tray). */
function resolveOpenTabInfo(sessionId: string): WorkspaceTabInfo | null {
  const s = useViewerStore.getState();
  const tabInfo = (
    tab: { role?: string } | undefined,
    kind: WorkspaceKind | undefined,
  ): WorkspaceTabInfo | null =>
    tab && kind ? { kind, isAgentTab: tab.role === GENERIC_AGENT_ROLE } : null;

  const home = selectSessionHome(s, sessionId);
  return home ? tabInfo(home.tab, home.workspace?.kind) : null;
}

export function makeRealOpenWorkspaceTabDeps(
  frameId?: string,
  pagePath?: string,
): OpenWorkspaceTabDeps {
  return {
    workspaceKindOf: (workspaceId) =>
      resolveWorkspaceTeam(useViewerStore.getState().workspaces, workspaceId)?.workspace.kind ??
      null,
    openTab: (workspaceId, role) => {
      const hit = resolveWorkspaceTeam(useViewerStore.getState().workspaces, workspaceId);
      if (!hit) return "";
      return useViewerStore.getState().openTabInWorkspace(hit.teamKey, workspaceId, { role });
    },
    closeTab: (_workspaceId, sessionId) =>
      // Roll back by the tab's unique sessionId — NOT by re-resolving the active
      // team, which can change during the awaited agent launch and strand the
      // orphan. Its host ends it wherever it landed (SW-GW-5).
      useViewerStore.getState().removeTerminalTabBySession(sessionId, { kill: true }),
    launchAgent: async (workspaceId, sessionId) => {
      const agentPath = await resolveAgentCliPath();
      if (!agentPath) {
        throw new Error("unavailable: agent CLI not found on PATH");
      }
      // The agent's MCP bridge and status hooks for this session, projected for the
      // workspace's dir and never written into it — the launcher's own path.
      const target = resolveWorkspaceTeam(useViewerStore.getState().workspaces, workspaceId);
      const cwd = target?.workspace.cwd;
      if ((frameId || pagePath) && !cwd) throw new Error("File not found");
      const scope = cwd
        ? await projectMcpScopeConfig(sessionId, cwd, null, undefined, [], {
            binary: agentBasename(agentPath),
            defaultArgs: true,
            ...(frameId || pagePath ? { frameId, pagePath, teamKey: target?.teamKey } : {}),
          })
        : { extraArgs: [] };
      // The panel's own shell: a relaunch restarts it the panel's way, with a fresh
      // config for the same dir.
      const spec = cwd ? { agentPath, configCwd: cwd } : { agentPath };
      const line = agentLaunchLine(spec, scope.extraArgs);
      const { submitted } = await submitLine(sessionId, line, "shell");
      if (submitted) recordAgentLaunch(sessionId, spec, line);
    },
  };
}

export function makeRealSendToWorkspaceTabDeps(): SendToWorkspaceTabDeps {
  return {
    tabInfo: resolveOpenTabInfo,
    // A shell command: gate on the shell being at its prompt, submit, report.
    sendCommand: (sessionId, text) => submitLine(sessionId, text, "shell"),
    agentLive: async (sessionId) => {
      // An uninstall can prevent a new launch while the existing process still runs.
      const agent =
        useAgentStateStore.getState().launchBySession[sessionId]?.agent ?? DEFAULT_AGENT_CLI;
      return isAgentConnected(sessionId, agent);
    },
    // A prompt into a CONFIRMED-LIVE agent: send unconditionally (gate:"none" —
    // the agent buffers stdin; a shell-idle gate would wrongly read a busy agent
    // as unavailable), submit, report.
    sendPrompt: (sessionId, text) => submitLine(sessionId, text, "none"),
  };
}
