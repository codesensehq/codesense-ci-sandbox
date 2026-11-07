// SPDX-License-Identifier: MIT
/**
 * Transport seam — the host-owned terminal layout: the snapshot, the layout
 * verbs, and the two layout events. A sanctioned `@/tauri/commands` importer
 * for the CLI workspace module (NFR5). The viewer store renders the host's
 * layout and changes it only through these verbs (server-authoritative: the
 * member answers every change on `terminal-runtime-event`, stamped with its
 * admitted target).
 *
 * Pass-through by design: error handling stays at the call sites.
 */

import { acceptsPrivateAuthority } from "@/shared/lib/private-terminal-authority";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  TERMINAL_LAYOUT_ADMISSION_CHANGED_EVENT,
  TERMINAL_LAYOUT_RESYNC_EVENT,
  TERMINAL_RUNTIME_EVENT,
  type RuntimeAdmissionRow,
  type RuntimeMemberTarget,
  type TerminalLayoutChanged,
  type TerminalLayoutResync,
  type TerminalRuntimeEvent,
} from "@/tauri/commands";

export {
  terminalLayoutMemberSnapshots,
  terminalOpenTab,
  terminalCloseTab,
  terminalRespawnTab,
  terminalPlaceTab,
  terminalRenameTab,
  terminalMoveTab,
  terminalSetActive,
  terminalSetGrid,
  terminalSetTags,
} from "@/tauri/commands";

export type {
  FrozenRuntimeLayout,
  ScopedExecution,
  RuntimeAdmissionRow,
  RuntimeLayoutReply,
  RuntimeMemberAddress,
  RuntimeMemberTarget,
  RuntimeNamespace,
  RuntimeSessionTarget,
  TerminalCloseMode,
  TerminalCloseOutcome,
  TerminalEntryState,
  TerminalGridPreset,
  TerminalGridState,
  TerminalLayoutChanged,
  TerminalLayoutResync,
  TerminalLayoutSnapshot,
  TerminalOpenTabRequest,
  TerminalPlacement,
  TerminalTabEntry,
  TerminalTabSpawn,
  TerminalRespawnScope,
  TerminalRespawnedTab,
  TerminalWorkspaceDescriptor,
  TerminalWorkspaceLayout,
} from "@/tauri/commands";

/** Listen for one member's changed workspace layout, with the member's
 *  admitted target. */
export function onTerminalLayoutChanged(
  handler: (
    target: RuntimeMemberTarget,
    change: TerminalLayoutChanged,
    privateAuthorityEpoch?: string,
  ) => void,
): Promise<UnlistenFn> {
  return listen<TerminalRuntimeEvent>(TERMINAL_RUNTIME_EVENT, ({ payload }) => {
    const change = payload.event.LayoutChanged;
    if (change && acceptsPrivateAuthority(payload.privateAuthorityEpoch))
      handler(payload.target, change, payload.privateAuthorityEpoch);
  });
}

/** Listen for a member's admission changing (admitted, re-admitted,
 *  historical, unavailable, removed). */
export function onTerminalAdmissionChanged(
  handler: (row: RuntimeAdmissionRow) => void,
): Promise<UnlistenFn> {
  return listen<RuntimeAdmissionRow>(
    TERMINAL_LAYOUT_ADMISSION_CHANGED_EVENT,
    (event) =>
      acceptsPrivateAuthority(event.payload.privateAuthorityEpoch) && handler(event.payload),
  );
}

/** Listen for a host whose layout moved while the app could not hear it. */
export function onTerminalLayoutResync(
  handler: (resync: TerminalLayoutResync) => void,
): Promise<UnlistenFn> {
  return listen<TerminalLayoutResync>(
    TERMINAL_LAYOUT_RESYNC_EVENT,
    (event) =>
      acceptsPrivateAuthority(event.payload.privateAuthorityEpoch) && handler(event.payload),
  );
}
