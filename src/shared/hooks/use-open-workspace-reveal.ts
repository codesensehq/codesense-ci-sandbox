// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

import { splitStoryHandle } from "@/features/dashboard/components/dashboard-launch";
import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import { logger } from "@/shared/lib/logger";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { showAgent } from "@/features/viewer/lib/show-agent";
import { resolveOpenWorkspaceMachine, type AgentSessionRow } from "@/tauri/commands";
import { OPEN_WORKSPACE_REVEAL_EVENT, type OpenWorkspaceRevealPayload } from "@/tauri/events";

/**
 * SW-WA-F5: show the story workspace an MCP `open_workspace` just opened in a
 * visible mode. Its agent's tab is already in the story's workspace in its
 * terminal host (the host key the viewer mints for that story), so revealing is
 * opening that workspace — focusing it only for a headed open. A named session
 * beside the story's own opens the same workspace. A running session no host
 * workspace shows is shown through the one Show (adopted into a tab).
 */
export function revealOpenedWorkspace(payload: OpenWorkspaceRevealPayload): Promise<void> {
  // A running session no host workspace shows (or a headless run): the one Show takes it
  // from here — focus, adopt into a tab, or open its run output.
  if (payload.target === "session" || payload.target === "runOutput") {
    if (!payload.sessionId) {
      logger.warn("mcp", "open_workspace reveal of a session is missing its session id");
      return Promise.resolve();
    }
    const row: AgentSessionRow = {
      handle: payload.handle,
      sessionId: payload.sessionId,
      kind: payload.target === "runOutput" ? "headless" : "dashboard-launched",
      alive: true,
      backgrounded: false,
      startedWith: null,
      startedAtMs: null,
      launcher: null,
      lastOutputMs: null,
      ...(payload.runId ? { provenance: { runId: payload.runId, storyId: "" } } : {}),
    };
    return showAgent(row, { teamKey: payload.teamKey, stayInTeam: true }).then(() => undefined);
  }
  if (payload.target === "team-workspace") {
    if (!payload.workspaceId || !payload.cwd) {
      logger.warn("mcp", "open_workspace reveal is missing its host workspace");
      return Promise.resolve();
    }
    useViewerStore.getState().openWorkspace({
      teamKey: payload.teamKey,
      workspaceId: payload.workspaceId,
      kind: payload.workspaceKind === "context" ? "context" : "generic",
      cwd: payload.cwd,
      focus: payload.focus,
      restoreSoftClosed: true,
      spawn: false,
    });
    if (payload.workspaceKind === "context") {
      useViewerStore.setState((s) => ({
        workspaces: {
          ...s.workspaces,
          [payload.teamKey]: (s.workspaces[payload.teamKey] ?? []).map((w) =>
            w.id === payload.workspaceId
              ? {
                  ...w,
                  contextWorktreeId: payload.workspaceId,
                  contextWorktreeName: payload.workspaceName,
                  contextWorktreeBranch: payload.branch,
                }
              : w,
          ),
        },
      }));
    }
    return Promise.resolve();
  }
  // A named session (`<storyId>#<name>`) is a tab in its story's workspace.
  const { storyId } = splitStoryHandle(payload.handle);
  // SW-FX-131 AC3: an open an MCP call is waiting on reports its outcome back to the backend
  // (success, or the reason it failed), not only to the log.
  const reply = (error: string | null) => {
    if (!payload.requestId) return;
    resolveOpenWorkspaceMachine(payload.requestId, error).catch((e: unknown) =>
      logger.warn("mcp", `open_workspace reply for ${payload.handle} failed: ${e}`),
    );
  };
  return useViewerStore
    .getState()
    .ensureStoryWorkspaceOpen(payload.teamKey, storyId, {
      focus: payload.focus,
      ...(payload.machineId
        ? { location: { kind: "machine" as const, machineId: payload.machineId } }
        : {}),
    })
    .then(
      (workspaceId) => {
        if (workspaceId) {
          reply(null);
          return;
        }
        const reason = `${storyId}'s workspace couldn't be opened in the viewer`;
        logger.warn("mcp", `open_workspace reveal of ${payload.handle} failed: ${reason}`);
        reply(reason);
      },
      (e: unknown) => {
        const reason = e instanceof Error ? e.message : String(e);
        logger.warn("mcp", `open_workspace reveal of ${payload.handle} failed: ${reason}`);
        reply(reason);
      },
    )
    .then(() => undefined);
}

/** Mount the reveal listener once at app root (App.tsx). */
export function useOpenWorkspaceReveal(): void {
  useEffect(() => {
    const unlisten = listen<OpenWorkspaceRevealPayload>(OPEN_WORKSPACE_REVEAL_EVENT, (event) => {
      void revealOpenedWorkspace(event.payload);
    });
    return () => {
      unlisten.then(safeUnlisten).catch(() => {});
    };
  }, []);
}
