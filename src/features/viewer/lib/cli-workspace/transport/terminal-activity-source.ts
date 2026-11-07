// SPDX-License-Identifier: MIT
/**
 * Transport seam — authoritative agent activity: each member host's activity
 * snapshot and the runtime/resync events that keep it current. A sanctioned
 * `@/tauri/commands` importer for the CLI workspace module (NFR5).
 *
 * Pass-through by design: recovery and error handling stay at the call sites.
 */

export {
  dismissAgentMark,
  terminalActivitySnapshot,
  TERMINAL_LAYOUT_RESYNC_EVENT,
  TERMINAL_RUNTIME_EVENT,
} from "@/tauri/commands";

export type {
  ActivitySnapshot,
  ActivitySnapshotReply,
  RuntimeMemberTarget,
  TerminalRuntimeEvent,
} from "@/tauri/commands";
