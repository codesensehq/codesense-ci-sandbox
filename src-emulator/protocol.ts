// SPDX-License-Identifier: MIT
import { version as xtermVersion } from "@xterm/headless/package.json";
import type { ReferenceBuffer } from "../src/shared/terminal/buffer-dump";
import type { TerminalSnapshot } from "./snapshot";

/**
 * Version of the message set carried inside the frames. The supervisor pins the
 * first worker's value and refuses a restarted worker that reports another one,
 * so bump it on any incompatible message change.
 */
export const WORKER_WIRE_VERSION = 2;

/** The `@xterm/headless` version bundled into this worker. */
export const XTERM_VERSION: string = xtermVersion;

/** A session's terminal, keyed by the host's number for it (stable across the
 *  session id changing at a warm-pool handoff). */
type SessionKey = { session: number };

/** `feed` / `text` / `dump` / `respawn` positions: `seq` is a byte offset over
 *  the session's whole output, `resize_gen` counts applied size changes. Both
 *  are owned by the host; the worker only records what it is told. */
export type CreateRequest = SessionKey & {
  type: "create";
  cols: number;
  rows: number;
  scrollback: number;
  seq_base: number;
  resize_gen_base: number;
  /** The host lost the output before `seq_base` (the worker was restarted). */
  history_lost: boolean;
};
export type FeedRequest = SessionKey & { type: "feed"; seq_end: number; data: string };
export type ResizeRequest = SessionKey & {
  type: "resize";
  cols: number;
  rows: number;
  resize_gen: number;
};
export type DumpRequest = SessionKey & { type: "dump"; id: number };
export type TextMode = "normal" | "screen";
export type TextRequest = SessionKey & { type: "text"; id: number; lines: number; mode: TextMode };
export type RespawnRequest = SessionKey & {
  type: "respawn";
  seq_base: number;
  resize_gen_base: number;
};
export type SnapshotRequest = SessionKey & { type: "snapshot"; id: number };
export type DisposeRequest = SessionKey & { type: "dispose" };
export type SessionRequest =
  | CreateRequest
  | FeedRequest
  | ResizeRequest
  | DumpRequest
  | TextRequest
  | SnapshotRequest
  | RespawnRequest
  | DisposeRequest;

export type HelloReply = {
  id: number;
  type: "hello";
  runtime: "node" | "bun";
  version: string;
  xterm_version: string;
  worker_wire_version: number;
};

/** Every session reply names the position it describes: the last parsed
 *  `seq_end` and the last applied `resize_gen`. */
type Position = { session: number; seq_end: number; resize_gen: number };

/** Sent, unasked, once a `feed` has been parsed; the host measures lag with it. */
export type AppliedReply = {
  type: "applied";
  session: number;
  seq_end: number;
  /** Milliseconds from `write` to its callback. */
  apply_ms: number;
};
export type DumpReply = ReferenceBuffer &
  Position & { id: number; type: "dump"; history_lost: boolean };
export type TextReply = Position & {
  id: number;
  type: "text";
  alt_screen: boolean;
  lines: string[];
};
/** The worker's memory after a forced collection: `rss` is what the machine
 *  pays, `live` the bytes still reachable (JS heap plus typed-array buffers). */
export type StatsReply = { id: number; type: "stats"; rss: number; live: number; sessions: number };
/** The snapshot's own `seq` / `resize_gen` are the position it describes. */
export type SnapshotReply = {
  id: number;
  type: "snapshot";
  session: number;
  snapshot: TerminalSnapshot;
};
export type ErrorReply = { id: number | null; type: "error"; message: string };

export type Reply =
  | HelloReply
  | AppliedReply
  | DumpReply
  | TextReply
  | SnapshotReply
  | StatsReply
  | ErrorReply;

export type RuntimeInfo = { runtime: "node" | "bun"; version: string };

export function currentRuntime(): RuntimeInfo {
  const bun = (globalThis as { Bun?: { version: string } }).Bun;
  if (bun) return { runtime: "bun", version: bun.version };
  return { runtime: "node", version: process.versions.node };
}

export function helloReply(id: number, runtime: RuntimeInfo): HelloReply {
  return {
    id,
    type: "hello",
    runtime: runtime.runtime,
    version: runtime.version,
    xterm_version: XTERM_VERSION,
    worker_wire_version: WORKER_WIRE_VERSION,
  };
}

/** A decoded request, or the error reply it earns. */
export type Decoded =
  | { kind: "hello"; id: number }
  | { kind: "stats"; id: number }
  | { kind: "session"; request: SessionRequest }
  | { kind: "error"; reply: ErrorReply };

const SESSION_TYPES = new Set([
  "create",
  "feed",
  "resize",
  "dump",
  "text",
  "snapshot",
  "respawn",
  "dispose",
]);
/** Requests the host waits on; the rest are fire-and-forget. */
const ANSWERED_TYPES = new Set(["hello", "stats", "dump", "text", "snapshot"]);

export function decodeRequest(payload: Uint8Array): Decoded {
  let request: unknown;
  try {
    request = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(payload));
  } catch (error) {
    return errorOf(null, `malformed request: ${String(error)}`);
  }
  if (typeof request !== "object" || request === null) {
    return errorOf(null, "request is not an object");
  }
  const fields = request as Record<string, unknown>;
  const type = fields.type;
  const id = safeInteger(fields.id);
  if (typeof type === "string" && ANSWERED_TYPES.has(type) && id === null) {
    return errorOf(null, "request has no numeric id");
  }
  if (type === "hello" || type === "stats") return { kind: type, id: id as number };
  if (typeof type === "string" && SESSION_TYPES.has(type)) {
    if (safeInteger(fields.session) === null) {
      return errorOf(id, `${type} has no numeric session`);
    }
    return { kind: "session", request: request as SessionRequest };
  }
  return errorOf(id, `unknown request type: ${JSON.stringify(type)}`);
}

function errorOf(id: number | null, message: string): Decoded {
  return { kind: "error", reply: { id, type: "error", message } };
}

function safeInteger(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) ? value : null;
}
