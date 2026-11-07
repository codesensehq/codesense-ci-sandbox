// SPDX-License-Identifier: MIT
import { VIRTUAL_TAB_SCHEME } from "@/shared/lib/virtual-viewer-registry";

/**
 * A remote doc tab's path: `virtual://machine-file/<machineId>/<abs path>`. The
 * machine rides IN the path so a remote tab can never be mistaken for (or read
 * as) a local file — the typed `HostPath{server_id, path}` of SW-RM-6, on the FE.
 * Virtual tabs are never persisted, so a remote buffer never outlives the session.
 */
export const MACHINE_FILE_PREFIX = `${VIRTUAL_TAB_SCHEME}machine-file`;

export const MB = 1024 * 1024;

/** Remote reads above this are gated ("Load first 1 MB · Open in terminal (less)"). */
export const REMOTE_SIZE_GATE_BYTES = MB;

export interface MachineFileRef {
  machineId: string;
  absPath: string;
}

export function machineFilePath({ machineId, absPath }: MachineFileRef): string {
  return `${MACHINE_FILE_PREFIX}/${encodeURIComponent(machineId)}${absPath}`;
}

export function parseMachineFilePath(path: string): MachineFileRef | null {
  if (!path.startsWith(`${MACHINE_FILE_PREFIX}/`)) return null;
  const rest = path.slice(MACHINE_FILE_PREFIX.length + 1);
  const slash = rest.indexOf("/");
  if (slash <= 0) return null;
  try {
    return { machineId: decodeURIComponent(rest.slice(0, slash)), absPath: rest.slice(slash) };
  } catch {
    return null;
  }
}

/** `/home/dev/src/x` → `~/src/x` for the machine's signed-in `username` (display
 *  only). Another user's home, or an unknown user, stays absolute. */
export function tildePath(absPath: string, username: string | null): string {
  if (!username) return absPath;
  for (const root of [`/home/${username}`, `/Users/${username}`]) {
    if (absPath === root) return "~";
    if (absPath.startsWith(`${root}/`)) return `~${absPath.slice(root.length)}`;
  }
  return absPath;
}

/** `hh:mm` wall-clock for "last reached" / "last loaded" — prefixed with the date
 *  when it isn't today, so yesterday's 14:02 never reads as today's. */
export function clockTime(ms: number, nowMs: number = Date.now()): string {
  if (!Number.isFinite(ms)) return "unknown";
  const at = new Date(ms);
  const time = at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (at.toDateString() === new Date(nowMs).toDateString()) return time;
  return `${at.toLocaleDateString([], { month: "short", day: "numeric" })} ${time}`;
}
