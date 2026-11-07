// SPDX-License-Identifier: MIT
import type { MachineFileRead } from "@/tauri/draft-commands";

/** A remote file as last read, and when. */
export type LoadedBuffer = { status: "ready"; read: MachineFileRead; at: number };

/** The session's remote buffers, keyed by machine + path: a tab switch remounts
 *  the viewer, and an open remote tab must keep its buffer while the machine is
 *  offline. In memory only — never persisted, never on disk — and bounded: the
 *  least recently loaded buffer goes first. */
const MAX_BUFFERS = 16;
const buffers = new Map<string, LoadedBuffer>();
const bufferKey = (machineId: string, absPath: string) => `${machineId}\u0000${absPath}`;

export function readBuffer(machineId: string, absPath: string): LoadedBuffer | undefined {
  return buffers.get(bufferKey(machineId, absPath));
}

export function rememberBuffer(machineId: string, absPath: string, loaded: LoadedBuffer): void {
  const key = bufferKey(machineId, absPath);
  buffers.delete(key);
  buffers.set(key, loaded);
  if (buffers.size > MAX_BUFFERS) buffers.delete(buffers.keys().next().value as string);
}

/** Drop every buffer read from `machineId` — once it is removed or unshared (here
 *  or by a teammate), its contents must not outlive the user's access (nor show
 *  under a new machine reusing the id). */
export function forgetMachineBuffers(machineId: string): void {
  const prefix = bufferKey(machineId, "");
  for (const key of [...buffers.keys()]) if (key.startsWith(prefix)) buffers.delete(key);
}
