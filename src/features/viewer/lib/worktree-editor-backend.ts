// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import {
  sourceRemoteFileHead,
  sourceRemoteFileInspect,
  sourceRemoteFileRead,
  sourceRemoteFileWrite,
  sourceRemoteListDirectory,
  sourceRemoteListFiles,
  worktreeFileHead,
  worktreeListFiles,
  writeWorktreeFile,
  type FileEntry,
} from "@/tauri/commands";
import type { Workspace } from "@/shared/stores/viewer-store";
import { isRemoteWorkspace, workspaceSourceRepo } from "@/shared/lib/workspace-location";

import type { RemoteFileAdapter } from "./use-file-editor";

/**
 * Everything the worktree editor needs from the filesystem, resolved once per
 * workspace so every pane, the tree and quick open speak to the same side —
 * this Mac's worktree, or a managed worktree on a remote machine (through the
 * terminal server's bounded file protocol). Callers never branch on location.
 *
 * Paths are ABSOLUTE throughout (the remote root re-anchored the way the local
 * tree already works); the remote wrappers translate to the root-relative paths
 * the machine speaks.
 */
export interface WorktreeEditorBackend {
  location: "local" | "remote";
  /** The worktree root (local cwd, or the remote directory). */
  rootPath: string;
  /** Lazy one-level lister for the tree; `undefined` = the local default. */
  listDirectory?: (dirPath: string) => Promise<FileEntry[]>;
  /** Quick open's file list (root-relative paths). */
  listFiles: () => Promise<{
    paths: string[];
    truncated: boolean;
    includesUntracked: boolean;
    branch: string | null;
  }>;
  /** The file's committed text: `null` for a new/untracked file, `undefined`
   *  when this side can't answer (an older remote host) — no gutter then. */
  readHead: (path: string) => Promise<string | null | undefined>;
  /** The save verb. `expectedHash` is the lock every write carries: local, the
   *  SHA-256 of the base text or `"absent"`; remote, the observation hash or
   *  `undefined` for "must not exist". */
  write: (path: string, content: string, expectedHash?: string) => Promise<void | { hash: string }>;
  /** Remote read/inspect adapter; `undefined` locally. */
  remote?: RemoteFileAdapter;
}

/** Base64 ↔ bytes. Chunked so a multi-megabyte file never spreads into
 *  `String.fromCharCode(...bytes)` (call-stack overflow). */
export function base64EncodeBytes(bytes: Uint8Array): string {
  let s = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    s += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(s);
}

export function base64DecodeToBytes(b64: string): Uint8Array {
  const s = atob(b64);
  const bytes = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) bytes[i] = s.charCodeAt(i);
  return bytes;
}

/** Decode remote bytes as strict UTF-8. A lenient decode would swap invalid
 *  bytes for U+FFFD and the next save would write that corruption back, so a
 *  non-UTF-8 file throws the typed error the editor shows as a placeholder.
 *  A leading BOM is KEPT (as U+FEFF): the default decoder strips it and the
 *  encoder never writes one, so a save would silently drop it. */
export function decodeUtf8Strict(bytes: Uint8Array): string {
  try {
    return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    throw { kind: "notUtf8" };
  }
}

/** `abs` relative to `root`. A path outside the root is refused: passed on
 *  unchanged it would be resolved against the root on the machine and name a
 *  different file. */
export function rootRelative(root: string, abs: string): string {
  if (abs === root) return "";
  if (abs.startsWith(`${root}/`)) return abs.slice(root.length + 1);
  throw { kind: "transportFailure", message: `${abs} is outside the worktree.` };
}

export function makeRemoteBackend(
  machineId: string,
  handle: string,
  rootPath: string,
  branch: string | null,
): WorktreeEditorBackend {
  const root = rootPath.replace(/\/+$/, "");
  const rel = (abs: string) => rootRelative(root, abs);
  const toAbsolute = (relPath: string) => (relPath === "" ? root : `${root}/${relPath}`);
  return {
    location: "remote",
    rootPath: root,
    listDirectory: async (dirPath) => {
      const entries = await sourceRemoteListDirectory(machineId, handle, rel(dirPath));
      return entries.map((e) => ({ ...e, path: toAbsolute(e.path) }));
    },
    listFiles: async () => {
      const list = await sourceRemoteListFiles(machineId, handle);
      return { ...list, branch };
    },
    readHead: async (path) => {
      const head = await sourceRemoteFileHead(machineId, handle, rel(path));
      if (!head.supported) return undefined;
      if (head.base64 == null) return null;
      try {
        return decodeUtf8Strict(base64DecodeToBytes(head.base64));
      } catch {
        return undefined; // committed bytes aren't text: no gutter
      }
    },
    write: async (path, content, expectedHash) => {
      const b64 = base64EncodeBytes(new TextEncoder().encode(content));
      const reply = await sourceRemoteFileWrite(machineId, handle, rel(path), b64, expectedHash);
      return { hash: reply.observation.hash };
    },
    remote: {
      readWithHash: async (path) => {
        const reply = await sourceRemoteFileRead(machineId, handle, rel(path));
        // The refusals carry the version read, so the poll doesn't read the
        // same unviewable version again on every tick.
        const hash = reply.observation.hash;
        // Part of a file is never editable content: a save would write the cut
        // buffer back over the whole file.
        if (reply.truncated) throw { kind: "boundsExceeded", hash };
        try {
          return { content: decodeUtf8Strict(base64DecodeToBytes(reply.base64)), hash };
        } catch {
          throw { kind: "notUtf8", hash };
        }
      },
      inspectHash: async (path) =>
        (await sourceRemoteFileInspect(machineId, handle, rel(path))).hash,
    },
  };
}

export function makeLocalBackend(rootPath: string): WorktreeEditorBackend {
  return {
    location: "local",
    rootPath: rootPath.replace(/\/+$/, ""),
    listFiles: async () => {
      const list = await worktreeListFiles(rootPath);
      return { ...list, includesUntracked: true };
    },
    readHead: (path) => worktreeFileHead(path),
    write: (path, content, expectedHash) => writeWorktreeFile(path, content, expectedHash),
  };
}

/** The backend for a workspace, stable across renders while its location is
 *  (keyed on primitives — the workspace record is replaced on every store
 *  update, and a fresh backend would refetch every pane's baseline). */
export function useWorktreeEditorBackend(ws: Workspace): WorktreeEditorBackend | null {
  const repo = workspaceSourceRepo(ws);
  const kind = repo?.kind ?? null;
  const machineId = repo?.kind === "remote" ? repo.machineId : null;
  const handle = repo?.kind === "remote" ? repo.handle : null;
  const remoteRoot = isRemoteWorkspace(ws) ? (ws.location.cwd ?? "") : "";
  const branch = isRemoteWorkspace(ws) ? (ws.location.branch ?? null) : null;
  const cwd = ws.cwd;
  return useMemo(
    () => backendFor({ kind, machineId, handle, remoteRoot, branch, cwd }),
    [kind, machineId, handle, remoteRoot, branch, cwd],
  );
}

/** The backend for a resolved workspace location, or `null` while it can't be
 *  served. A remote workspace NEVER falls back to this Mac: without a minted
 *  machine + handle it has no backend (a local one would read and write this
 *  Mac's disk at the remote path). A local one needs its cwd. */
export function backendFor(loc: {
  kind: "local" | "remote" | null;
  machineId: string | null;
  handle: string | null;
  remoteRoot: string;
  branch: string | null;
  cwd: string | null | undefined;
}): WorktreeEditorBackend | null {
  if (loc.kind == null) return null; // remote workspace whose handle isn't minted yet
  if (loc.kind === "remote") {
    // No remote directory yet (a machine location without its cwd): there is
    // no backend until it arrives. A worktree AT "/" is a real root, though —
    // it trims to "" but paths still anchor correctly ("/" + rel).
    return loc.machineId && loc.handle && loc.remoteRoot.trim() !== ""
      ? makeRemoteBackend(loc.machineId, loc.handle, loc.remoteRoot, loc.branch)
      : null;
  }
  return loc.cwd ? makeLocalBackend(loc.cwd) : null;
}
