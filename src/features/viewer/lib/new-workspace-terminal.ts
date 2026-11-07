// SPDX-License-Identifier: MIT
export const NEW_WORKSPACE_TERMINAL_EVENT = "codesense:new-terminal";

/** What the chooser opens with chosen: a CLI, the folder it starts in, and the machine. */
export interface NewTerminalStart {
  launch: string;
  cwd: string | null;
  machineId: string;
}

/** The detail of {@link NEW_WORKSPACE_TERMINAL_EVENT}. */
export type NewWorkspaceTerminalDetail = {
  teamKey: string;
  workspaceId: string;
} & Partial<NewTerminalStart>;

/** How long a request waits for its workspace's chooser to mount (its workspace being shown). */
const PENDING_MS = 10_000;

/** Requests whose workspace had no chooser mounted yet, by workspace. */
const pending = new Map<string, { detail: NewWorkspaceTerminalDetail; at: number }>();
const scope = (teamKey: string, workspaceId: string) => JSON.stringify([teamKey, workspaceId]);

/**
 * The current workspace owns the machine/directory chooser. `start` preselects its choices. The
 * chooser mounts only while its workspace is shown, so the request also waits, briefly, for a
 * chooser that mounts after the workspace is revealed.
 */
export function requestNewWorkspaceTerminal(
  teamKey: string,
  workspaceId: string,
  start?: NewTerminalStart,
): void {
  const detail: NewWorkspaceTerminalDetail = { teamKey, workspaceId, ...start };
  pending.set(scope(teamKey, workspaceId), { detail, at: Date.now() });
  window.dispatchEvent(new CustomEvent(NEW_WORKSPACE_TERMINAL_EVENT, { detail }));
}

/** Take the request waiting for this workspace's chooser, if one is still fresh. Once only. */
export function takeNewWorkspaceTerminalRequest(
  teamKey: string,
  workspaceId: string,
): NewWorkspaceTerminalDetail | null {
  const key = scope(teamKey, workspaceId);
  const entry = pending.get(key);
  pending.delete(key);
  return entry && Date.now() - entry.at < PENDING_MS ? entry.detail : null;
}
