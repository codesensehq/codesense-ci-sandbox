// SPDX-License-Identifier: MIT
/**
 * Where a workspace lives. A workspace without a `location` lives on This Mac
 * (its `cwd` is a local directory). One with a machine location lives on that
 * machine: its worktree, terminals and agents run there, the record keeps the
 * managed handle and the remote directory, and its local `cwd` stays empty so
 * nothing local ever treats the remote directory as a path on this Mac.
 *
 * The store reaches the machine through a driver the machines feature
 * registers, so the store never imports the machines feature (which imports
 * the store).
 */

/** A workspace that lives on a remote machine. Set once, at create. */
export interface MachineLocation {
  kind: "machine";
  machineId: string;
  /** Minted once per (machine, team, workspace) and kept, so a repeated or
   *  interrupted create reconciles the same preparation, never a second one. */
  requestId: string;
  /** The base and branch the machine prepared (or is preparing) the worktree
   *  from: replayed as they were whenever the same request is repeated, so a
   *  story whose branch or base changed since still reconciles its worktree. */
  baseRef?: string;
  branch?: string;
  /** The managed workspace handle, once the machine has prepared it. */
  handle?: string;
  /** The worktree on the machine, where the workspace's terminals open. */
  cwd?: string;
}

export type WorkspaceLocation = MachineLocation;

/** What a create was asked to do: This Mac, or a machine. */
export type WorkspaceLocationChoice = { kind: "local" } | { kind: "machine"; machineId: string };

/** Whether `ws` lives on a remote machine. */
export function isRemoteWorkspace<T extends { location?: WorkspaceLocation }>(
  ws: T | null | undefined,
): ws is T & { location: MachineLocation } {
  return ws?.location?.kind === "machine";
}

/**
 * DESIGN-remote-source.md §3.6 — compute the `SourceRepo` identity of a
 * workspace once, so the overlay / inset / watcher threads a single object
 * through every hook instead of branching on `isRemoteWorkspace(ws)` at every
 * callsite. Returns:
 *   - `{kind: "local", cwd}` for a workspace on This Mac,
 *   - `{kind: "remote", machineId, handle}` for one on a machine (handle
 *     always set — the caller guards on a remote story whose handle is still
 *     pending by returning `null` from `useTabSourceRepo`),
 *   - `null` for a remote workspace whose `handle` hasn't been minted yet
 *     (the caller treats this as "still resolving" and hides the affordance).
 *
 * The TYPE from `./workspace-location` depends on the Workspace shape in the
 * viewer store, but we avoid importing the store (which would cause a cycle);
 * the structural `{cwd?: string, location?: WorkspaceLocation}` lets both call.
 */
export type WorkspaceSourceRepo =
  | { kind: "local"; cwd: string }
  | { kind: "remote"; machineId: string; handle: string };

export function workspaceSourceRepo<T extends { cwd?: string; location?: WorkspaceLocation }>(
  ws: T,
): WorkspaceSourceRepo | null {
  if (isRemoteWorkspace(ws)) {
    if (!ws.location.handle) return null;
    return { kind: "remote", machineId: ws.location.machineId, handle: ws.location.handle };
  }
  return { kind: "local", cwd: ws.cwd ?? "" };
}

/** What the store asks of the machines feature for a remote story workspace. */
export interface RemoteWorkspaceDriver {
  /** Ask where a new story workspace should live. Resolves `{kind:"local"}` at
   *  once when the team has no machines; `null` when the person cancelled. */
  chooseLocation(teamKey: string, label: string): Promise<WorkspaceLocationChoice | null>;
  /** Connect the machine if needed, then create (or reconcile) the workspace
   *  there. `onProgress` gets each Step 1 reading as the setup line's state. */
  prepare(input: {
    teamKey: string;
    workspaceId: string;
    storyId: string;
    machineId: string;
    requestId: string;
    sourceRepo: string;
    baseRef: string;
    branch: string;
    onProgress: (
      state: import("@/features/viewer/lib/workspace-setup-state").WorkspaceSetupState,
    ) => void;
  }): Promise<{ handle: string; cwd: string; requestId: string; baseRef: string; branch: string }>;
  /** Open a terminal in the workspace on its machine, at its worktree.
   *  `first`: only when the workspace has no terminal yet (an open of the
   *  workspace), connecting the machine first if needed. */
  openTerminal(input: {
    teamKey: string;
    workspaceId: string;
    location: MachineLocation;
    first?: boolean;
  }): void;
  /** Start an agent in the workspace on its machine, at its worktree,
   *  connecting first if needed. Resolves the tab's session once it opened; rejects with the
   *  machine's reason, which the workspace also shows with what to do. */
  launchAgent(input: {
    frameId?: string;
    pagePath?: string;
    teamKey: string;
    workspaceId: string;
    location: MachineLocation;
    /** The agent to start: a key, or an id in the workspace's team. */
    agentKey: string;
    /** A named session's tab name; omitted for the story's main session. */
    name?: string;
    /** `false`: start it in the background, never taking the focus. */
    headed?: boolean;
    /** `"focus"` (the default) reuses the same agent already running there;
     *  `"new"` always starts another. */
    sessionMode?: "focus" | "new";
    /** The agent's first prompt, in place of its own: a new agent starts with it. */
    initialPrompt?: string;
  }): Promise<{ sessionId: string; fresh: boolean }>;
}

let driver: RemoteWorkspaceDriver | null = null;

/** Install the driver; returns its removal. */
export function registerRemoteWorkspaceDriver(next: RemoteWorkspaceDriver): () => void {
  driver = next;
  return () => {
    if (driver === next) driver = null;
  };
}

/** The installed driver, or `null` in a build (or test) without machines. */
export function remoteWorkspaceDriver(): RemoteWorkspaceDriver | null {
  return driver;
}
