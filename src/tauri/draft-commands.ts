// SPDX-License-Identifier: MIT
import { invoke } from "@tauri-apps/api/core";

import type { FileEntry } from "@/tauri/commands";

/**
 * DRAFT COMMAND CONTRACTS
 * =======================
 * Typed wrappers for backend commands the settled mock-first Machines UI needs but
 * that do NOT yet have a Rust handler. They are deliberately kept OUT of
 * `commands.ts` so the `commands.contract.test.ts` count (TS wrappers ↔ Rust
 * handlers) stays exact.
 *
 * Each is real, typed, and invoked by production UI; in `pnpm dev:mock` the mock
 * backend (`src/app/mock/handlers/machines.ts`) answers them. In a real build they
 * reject as not-registered until the Rust handler lands — that's the "draft" state.
 * When a wiring story implements a handler, move the wrapper into `commands.ts`,
 * register the command in `lib.rs` and bump `EXPECTED_*_COMMAND_COUNT`.
 *
 * The machine registry, the SSH connector, the Add-machine checklist and the
 * install are real (`commands.ts`, "Remote Machines"). What stays here:
 *   - SW-RM-5 — server-keyed layout projection (get_machine_layout_snapshot,
 *               open_machine_terminal)
 *   - SW-RM-6 — remote file view (machine_fs_list, machine_fs_read)
 */

// ---------------------------------------------------------------------------
// Layout snapshot (SW-RM-5)
// ---------------------------------------------------------------------------

/** One workspace in a machine's server-owned layout (the RT-7a descriptor subset
 *  the census tiles render). */
export interface MachineWorkspaceDescriptor {
  workspaceId: string;
  name: string;
  /** Absolute worktree path ON THE MACHINE. */
  worktreePath: string;
  branch: string | null;
  tabs: string[];
}

/** A terminal tab that ended because its machine restarted (the epoch changed). */
export interface MachineEndedTab {
  workspaceName: string;
  tab: string;
  /** Where "Reopen workspace" opens a fresh terminal, on the machine. */
  cwd: string;
}

export interface MachineLayoutSnapshot {
  machineId: string;
  serverId: string;
  epoch: number;
  workspaces: MachineWorkspaceDescriptor[];
  /** Tabs that ended because the machine restarted since this client last saw it
   *  (the epoch changed). Empty when the epoch is unchanged. */
  endedByRestart: MachineEndedTab[];
}

/** SW-RM-5 — the machine's server-owned layout; for an unreachable machine the
 *  last-known census (never removed while unreachable). */
export const getMachineLayoutSnapshot = (machineId: string) =>
  invoke<MachineLayoutSnapshot>("get_machine_layout_snapshot", { machineId });

/** SW-RM-5 — open a new terminal tab ON the machine (`command` null = a shell).
 *  "Open in terminal" runs `$EDITOR <file>` / `less <file>` through here. Never
 *  falls back to This Mac. */
export const openMachineTerminal = (machineId: string, cwd: string, command: string | null) =>
  invoke<{ tabId: string }>("open_machine_terminal", { machineId, cwd, command });

// ---------------------------------------------------------------------------
// Remote file view (SW-RM-6)
// ---------------------------------------------------------------------------

/** SW-RM-6 — list one directory on the machine, through its terminal server
 *  (no second SSH/SFTP channel). Same `FileEntry` shape as `list_directory`. */
export const machineFsList = (machineId: string, dirPath: string) =>
  invoke<FileEntry[]>("machine_fs_list", { machineId, dirPath });

export interface MachineFileRead {
  content: string;
  /** Full size on the machine, in bytes. */
  size: number;
  /** True when `size > maxBytes` and only the first `maxBytes` came back. */
  truncated: boolean;
}

/** SW-RM-6 — read up to `maxBytes` of a file on the machine. */
export const machineFsRead = (machineId: string, path: string, maxBytes: number) =>
  invoke<MachineFileRead>("machine_fs_read", { machineId, path, maxBytes });
