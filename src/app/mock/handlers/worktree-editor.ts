// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { FileEntry } from "@/tauri/commands";

/**
 * The worktree code editor's backend in `dev:mock`: an in-memory story worktree
 * (a small TypeScript project with a committed `HEAD` that differs from the
 * working copy, so the change gutter has something to show), served both as a
 * LOCAL worktree (`list_directory` / `read_file` / `write_worktree_file` under a
 * `/worktrees/` root) and as a REMOTE one (the `source_remote_*` commands, any
 * handle). Wraps the content-file `list_directory` so context paths still work.
 *
 * Edge worlds from the URL (`?harness=worktree-editor` drives them):
 * - `stress=1` adds 20,000 generated files (quick-open ranking/speed) and a
 *   5 MB log (the too-large state);
 * - `latency=<ms>` delays every remote reply (loading/saving indicators);
 * - `offline=1` makes every remote call fail as a disconnected machine.
 */

const LOCAL_ROOT_RE = /^(.*\/worktrees\/[^/]+)(?:\/(.*))?$/;

const SESSION_TS = `import { retry } from "../lib/retry";

/** How long a signed-in session stays valid without activity. */
export const SESSION_TTL_MS = 30 * 60 * 1000;

export interface Session {
  token: string;
  userId: string;
  expiresAt: number;
}

export class SessionStore {
  private sessions = new Map<string, Session>();

  constructor(private readonly now: () => number = Date.now) {}

  open(userId: string, token: string): Session {
    const session = { token, userId, expiresAt: this.now() + SESSION_TTL_MS };
    this.sessions.set(token, session);
    return session;
  }

  /** Extend a live session; expired ones are dropped instead. */
  touch(token: string): Session | null {
    const session = this.sessions.get(token);
    if (!session) return null;
    if (session.expiresAt <= this.now()) {
      this.sessions.delete(token);
      return null;
    }
    session.expiresAt = this.now() + SESSION_TTL_MS;
    return session;
  }

  async refresh(token: string, fetchToken: () => Promise<string>): Promise<Session | null> {
    const current = this.touch(token);
    if (!current) return null;
    const next = await retry(fetchToken, { attempts: 3, backoffMs: 250 });
    this.sessions.delete(token);
    return this.open(current.userId, next);
  }
}
`;

const SESSION_TS_HEAD = SESSION_TS.replace("30 * 60 * 1000", "15 * 60 * 1000")
  .replace(
    `  async refresh(token: string, fetchToken: () => Promise<string>): Promise<Session | null> {
    const current = this.touch(token);
    if (!current) return null;
    const next = await retry(fetchToken, { attempts: 3, backoffMs: 250 });
    this.sessions.delete(token);
    return this.open(current.userId, next);
  }
`,
    "",
  )
  .replace(
    `  /** Extend a live session; expired ones are dropped instead. */
  touch(token: string): Session | null {
    const session = this.sessions.get(token);
    if (!session) return null;
    if (session.expiresAt <= this.now()) {
      this.sessions.delete(token);
      return null;
    }
`,
    `  touch(token: string): Session | null {
    const session = this.sessions.get(token);
    if (!session) return null;
    // TODO: expire stale sessions
`,
  );

const RETRY_TS = `export interface RetryOptions {
  attempts: number;
  backoffMs: number;
}

/** Run \`fn\` until it resolves, waiting longer after each failure. */
export async function retry<T>(fn: () => Promise<T>, opts: RetryOptions): Promise<T> {
  let lastError: unknown;
  for (let attempt = 0; attempt < opts.attempts; attempt++) {
    try {
      return await fn();
    } catch (error) {
      lastError = error;
      await new Promise((resolve) => setTimeout(resolve, opts.backoffMs * 2 ** attempt));
    }
  }
  throw lastError;
}
`;

const SEED: Record<string, string> = {
  ".gitignore": "node_modules/\ndist/\n",
  "README.md":
    "# Login service\n\nSessions, token refresh and the retry helper the API client uses.\n\n## Develop\n\n```sh\npnpm install\npnpm test\n```\n",
  "package.json": `{
  "name": "login-service",
  "version": "1.4.0",
  "type": "module",
  "scripts": {
    "test": "vitest run",
    "build": "tsc -p ."
  }
}
`,
  "src/index.ts": `export { SessionStore, SESSION_TTL_MS } from "./auth/session";\nexport { retry } from "./lib/retry";\n`,
  "src/auth/session.ts": SESSION_TS,
  "src/auth/session.test.ts": `import { describe, expect, it } from "vitest";\nimport { SessionStore } from "./session";\n\ndescribe("SessionStore", () => {\n  it("expires idle sessions", () => {\n    let now = 0;\n    const store = new SessionStore(() => now);\n    store.open("u1", "t1");\n    now = 31 * 60 * 1000;\n    expect(store.touch("t1")).toBeNull();\n  });\n});\n`,
  "src/lib/retry.ts": RETRY_TS,
  "styles/app.css": `:root {\n  --brand: #0969da;\n}\n\n.login-form {\n  display: grid;\n  gap: 0.75rem;\n}\n`,
  "docs/windows-setup.txt": "Run setup.ps1 from PowerShell.\r\nThen open the app.\r\n",
};

const HEAD: Record<string, string> = {
  ...SEED,
  "src/auth/session.ts": SESSION_TS_HEAD,
};
// A file the agent created this session: no committed version yet.
delete HEAD["src/lib/retry.ts"];

/** 20,000 plausible paths across 40 packages (quick open at monorepo scale). */
export function stressPaths(count = 20_000): string[] {
  const parts = ["components", "hooks", "lib", "stores", "utils", "views", "api", "models"];
  const words = ["session", "button", "token", "retry", "panel", "router", "cache", "query"];
  const out: string[] = [];
  for (let i = 0; i < count; i++) {
    const pkg = `packages/pkg-${String(i % 40).padStart(2, "0")}`;
    const dir = parts[i % parts.length];
    const word = words[Math.floor(i / 8) % words.length];
    out.push(`${pkg}/src/${dir}/${word}-${i}.ts`);
  }
  return out;
}

/** ~5 MB of log lines: over the editor's size gate. */
function bigLog(): string {
  return Array.from(
    { length: 60_000 },
    (_, i) =>
      `2026-10-05T12:00:${String(i % 60).padStart(2, "0")}Z INFO request ${i} served in ${i % 97}ms by worker ${i % 8}`,
  ).join("\n");
}

function seedFor(search: string): Record<string, string> {
  if (new URLSearchParams(search).get("stress") !== "1") return SEED;
  const extra: Record<string, string> = { "logs/server-5mb.log": bigLog() };
  for (const path of stressPaths()) extra[path] = `export const id = ${JSON.stringify(path)};\n`;
  return { ...SEED, ...extra };
}

/** One worktree's working copy, created on first touch. */
const trees = new Map<string, Map<string, string>>();
let seedSearch = "";
function treeFor(root: string): Map<string, string> {
  let tree = trees.get(root);
  if (!tree) {
    tree = new Map(Object.entries(seedFor(seedSearch)));
    trees.set(root, tree);
  }
  return tree;
}

function listOf(
  tree: Map<string, string>,
  dir: string,
): { name: string; isDir: boolean; size: number | null }[] {
  const prefix = dir ? `${dir}/` : "";
  const dirs = new Set<string>();
  const files: { name: string; size: number }[] = [];
  for (const [path, content] of tree) {
    if (!path.startsWith(prefix)) continue;
    const rest = path.slice(prefix.length);
    const slash = rest.indexOf("/");
    if (slash === -1) files.push({ name: rest, size: new TextEncoder().encode(content).length });
    else dirs.add(rest.slice(0, slash));
  }
  if (dirs.size === 0 && files.length === 0 && dir)
    throw new Error(`No such file or directory: ${dir}`);
  const byName = (a: { name: string }, b: { name: string }) =>
    a.name.toLowerCase().localeCompare(b.name.toLowerCase());
  return [
    ...[...dirs].map((name) => ({ name, isDir: true, size: null })).sort(byName),
    ...files.map((f) => ({ ...f, isDir: false })).sort(byName),
  ];
}

function entry(
  absDir: string,
  e: { name: string; isDir: boolean; size: number | null },
): FileEntry {
  const dot = e.name.lastIndexOf(".");
  return {
    name: e.name,
    path: absDir ? `${absDir}/${e.name}` : e.name,
    is_dir: e.isDir,
    extension: !e.isDir && dot > 0 ? e.name.slice(dot + 1) : null,
    size: e.size,
  };
}

async function sha256(text: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}

const toB64 = (text: string) => {
  const bytes = new TextEncoder().encode(text);
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
};
const fromB64 = (b64: string) =>
  // ignoreBOM keeps a BOM, matching the real remote backend's decode.
  new TextDecoder("utf-8", { ignoreBOM: true }).decode(
    Uint8Array.from(atob(b64), (c) => c.charCodeAt(0)),
  );

function split(abs: string): { root: string; rel: string } | null {
  const m = LOCAL_ROOT_RE.exec(abs);
  return m ? { root: m[1]!, rel: m[2] ?? "" } : null;
}

/** Wrap every remote handler with the URL's latency / offline world. */
function remoteWorld(search: string, handlers: HandlerRegistry): HandlerRegistry {
  const params = new URLSearchParams(search);
  const latency = Number(params.get("latency") ?? 0) || 0;
  const offline = params.get("offline") === "1";
  if (!latency && !offline) return handlers;
  const out: HandlerRegistry = {};
  for (const [name, fn] of Object.entries(handlers)) {
    if (!name.startsWith("source_remote_")) {
      out[name] = fn;
      continue;
    }
    out[name] = async (args) => {
      if (latency) await new Promise((r) => setTimeout(r, latency));
      if (offline && !name.startsWith("source_remote_watch")) throw { kind: "disconnected" };
      return fn(args);
    };
  }
  return out;
}

export function worktreeEditorHandlers(base: HandlerRegistry, search = ""): HandlerRegistry {
  seedSearch = search;
  const remoteRoot = (handle: string) => `remote:${handle}`;
  return remoteWorld(search, {
    list_directory: (args) => {
      const dirPath = args.dirPath as string;
      const at = split(dirPath);
      if (!at) return base.list_directory?.(args);
      return listOf(treeFor(at.root), at.rel).map((e) => entry(dirPath, e));
    },
    read_file: (args) => {
      const at = split(args.path as string);
      if (!at) throw new Error(`No such file or directory: ${args.path as string}`);
      const content = treeFor(at.root).get(at.rel);
      if (content == null) throw new Error(`No such file or directory (os error 2)`);
      return content;
    },
    write_worktree_file: async (args) => {
      const at = split(args.path as string);
      if (!at) throw new Error("Access denied: path is outside any registered worktree");
      const tree = treeFor(at.root);
      const expected = args.expectedSha256 as string | undefined;
      const current = tree.get(at.rel);
      if (expected === "absent" && current != null) {
        throw `conflict: ${args.path as string} was created since it was deleted`;
      }
      if (
        expected &&
        expected !== "absent" &&
        (current == null || (await sha256(current)) !== expected)
      ) {
        throw `conflict: ${args.path as string} changed on disk since it was opened`;
      }
      tree.set(at.rel, args.content as string);
      return undefined;
    },
    worktree_list_files: (args) => {
      const at = split(args.root as string);
      const tree = treeFor(at?.root ?? (args.root as string));
      return {
        paths: [...tree.keys()].sort(),
        truncated: false,
        branch: "story/BUG-142-login-timeout",
      };
    },
    worktree_file_head: (args) => {
      const at = split(args.path as string);
      return at ? (HEAD[at.rel] ?? null) : null;
    },
    lazy_attach_worktree_watch: () => 1,
    detach_worktree_watch: () => undefined,

    // The same worktree reached through a remote machine's terminal server.
    source_remote_list_directory: (args) =>
      listOf(treeFor(remoteRoot(args.handle as string)), args.path as string).map((e) =>
        entry(args.path as string, e),
      ),
    source_remote_file_read: async (args) => {
      const content = treeFor(remoteRoot(args.handle as string)).get(args.path as string);
      if (content == null) throw { kind: "notFound" };
      return {
        base64: toB64(content),
        observation: { hash: await sha256(content), mtimeNs: 0, size: content.length, mode: 420 },
        truncated: false,
      };
    },
    source_remote_file_write: async (args) => {
      const tree = treeFor(remoteRoot(args.handle as string));
      const current = tree.get(args.path as string);
      const expected = args.expectedHash as string | undefined;
      // Parity with the real side: no hash means "expect absent", so an
      // unlocked write over an existing file is a conflict; a hash for a file
      // that's gone is NotFound.
      if (current == null && expected) throw { kind: "notFound" };
      if (current != null && (!expected || (await sha256(current)) !== expected)) {
        throw { kind: "conflict", observation: { hash: await sha256(current) } };
      }
      const next = fromB64(args.base64 as string);
      tree.set(args.path as string, next);
      return {
        observation: { hash: await sha256(next), mtimeNs: 1, size: next.length, mode: 420 },
      };
    },
    source_remote_file_inspect: async (args) => {
      const content = treeFor(remoteRoot(args.handle as string)).get(args.path as string);
      if (content == null) throw { kind: "notFound" };
      return { hash: await sha256(content), mtimeNs: 0, size: content.length, mode: 420 };
    },
    source_remote_file_head: (args) => {
      const head = HEAD[args.path as string];
      return { base64: head == null ? null : toB64(head), supported: true };
    },
    source_remote_list_files: (args) => ({
      paths: [...treeFor(remoteRoot(args.handle as string)).keys()].sort(),
      truncated: false,
      includesUntracked: true,
    }),
    source_remote_watch_start: () => 1,
    source_remote_watch_stop: () => undefined,
    source_remote_tracked_count: () => 9,
  });
}
