// SPDX-License-Identifier: MIT
/**
 * SW-WL-3 (F-2): transition seam between the `WorkspaceOpenSet` refcount in
 * `viewer-store` and the Rust surface-bound watcher.
 *
 * The store fires `fireWorkspaceOpened` on a workspace's 0→1 open transition
 * and `fireWorkspaceClosed` on its 1→0 transition. T2 registers hooks that
 * build the `WatchConfig` for the workspace and invoke the `lazy_attach_workspace`
 * / `lazy_detach_workspace` Tauri commands — there is deliberately NO new
 * frontend→Rust event substrate (F-2: command-driven). Until hooks are
 * registered the transitions are no-ops, so T1 ships and is unit-testable in
 * isolation; tests register spies via `registerWorkspaceWatchHooks`.
 */
export type WorkspaceWatchTransition = (teamKey: string, workspaceId: string) => void;

interface WorkspaceWatchHooks {
  onWorkspaceOpened: WorkspaceWatchTransition;
  onWorkspaceClosed: WorkspaceWatchTransition;
}

const NOOP: WorkspaceWatchTransition = () => {};

let hooks: WorkspaceWatchHooks = {
  onWorkspaceOpened: NOOP,
  onWorkspaceClosed: NOOP,
};

/** Register the attach/detach hooks (T2 wires the real Tauri invokers here). */
export function registerWorkspaceWatchHooks(next: WorkspaceWatchHooks): void {
  hooks = next;
}

/** Restore the default no-op hooks (test isolation). */
export function resetWorkspaceWatchHooks(): void {
  hooks = { onWorkspaceOpened: NOOP, onWorkspaceClosed: NOOP };
}

/** Fired by the store on a 0→1 open transition for `workspaceId`. */
export function fireWorkspaceOpened(teamKey: string, workspaceId: string): void {
  hooks.onWorkspaceOpened(teamKey, workspaceId);
}

/** Fired by the store on a 1→0 close transition for `workspaceId`. */
export function fireWorkspaceClosed(teamKey: string, workspaceId: string): void {
  hooks.onWorkspaceClosed(teamKey, workspaceId);
}
