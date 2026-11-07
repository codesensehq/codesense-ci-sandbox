// SPDX-License-Identifier: MIT
import { globMatcherWithCase } from "@/features/agent-context/lib/agent-context-model";
import { joinArgs, splitArgs } from "@/shared/lib/tokenize-args";
import FILTER_GLOBS from "../../../../src-tauri/src/agents/generated/context_filter_globs.json";
import type {
  AgentDefinition,
  AgentForm,
  AgentLaunchDefault,
  AgentPermissions,
  AgentRuntime,
} from "@/tauri/commands";

/**
 * The agent as the editor holds it, and its round trip to the `agent_save` form. Every field
 * is kept as the inputs hold it (args as typed, permission modes as `""` when unset); the
 * form the backend receives is derived, so two drafts are the same edit exactly when their
 * forms are equal.
 *
 * Permissions are each CLI's own config, kept verbatim: rule strings are never parsed here.
 * Every runtime's block is kept, so switching runtime and back restores the first one, and
 * keys the editor does not show (Claude's `additional_directories`, …) ride along untouched.
 */

/** Claude's documented permission modes. */
export const CLAUDE_MODES = ["default", "acceptEdits", "plan", "dontAsk", "bypassPermissions"];
/** Codex's documented `approval_policy` values. */
export const CODEX_APPROVAL_POLICIES = ["untrusted", "on-failure", "on-request", "never"];
/** Codex's documented `sandbox_mode` values. */
export const CODEX_SANDBOX_MODES = ["read-only", "workspace-write", "danger-full-access"];
/** What a headless Codex launch gets for a value the agent leaves unset. */
export const CODEX_HEADLESS_DEFAULT_HINT = "headless default: never · workspace-write";

type ClaudeBlock = NonNullable<AgentPermissions["claude"]>;
type CodexBlock = NonNullable<AgentPermissions["codex"]>;
type OpenCodeBlock = NonNullable<AgentPermissions["opencode"]>;

export interface ClaudeDraft {
  allow: string[];
  ask: string[];
  deny: string[];
  /** `""`: unset. */
  defaultMode: string;
  /** The block's keys the editor does not show, kept as they are. */
  rest: Omit<ClaudeBlock, "allow" | "ask" | "deny" | "default_mode">;
}

export interface CodexDraft {
  /** `""`: unset. */
  approvalPolicy: string;
  sandboxMode: string;
  rest: Omit<CodexBlock, "approval_policy" | "sandbox_mode">;
}

export interface OpenCodeDraft {
  permission: { pattern: string; decision: string }[];
  rest: Omit<OpenCodeBlock, "permission">;
}

/** Every field the editor edits, as the inputs hold it. */
export interface AgentDraft {
  name: string;
  description: string;
  runtime: AgentRuntime;
  /** The args as one line, shell-quoted (`joinArgs`); split back into the same argv words on
   *  save, so a word with a space or a quote in it survives an edit. */
  args: string;
  startupSettings?: Record<string, unknown> | null;
  launch: AgentLaunchDefault;
  mcpServers: string[];
  /** Stored bridge policy, retained even when the editor changes runtime. */
  mcpBridgeTools: string[] | null;
  startup: string[];
  defaultPrompt: string;
  role: string | null;
  /** Relaunched with a resume brief after a terminal server update (SW-AGR-19 PR-6). */
  resumeAfterRestart: boolean;
  claude: ClaudeDraft;
  codex: CodexDraft;
  opencode: OpenCodeDraft;
  /** Read-only native settings, retained verbatim through edits and runtime switches. */
  hermes?: AgentPermissions["hermes"];
}

/** Where a new agent goes. */
export type AgentPlacement = { scope: "personal" } | { scope: "team"; org: string; team: string };

function claudeDraft(block: AgentPermissions["claude"]): ClaudeDraft {
  const { allow, ask, deny, default_mode, ...rest } = block ?? {};
  return {
    allow: allow ?? [],
    ask: ask ?? [],
    deny: deny ?? [],
    defaultMode: default_mode ?? "",
    rest,
  };
}

function codexDraft(block: AgentPermissions["codex"]): CodexDraft {
  const { approval_policy, sandbox_mode, ...rest } = block ?? {};
  return { approvalPolicy: approval_policy ?? "", sandboxMode: sandbox_mode ?? "", rest };
}

function opencodeDraft(block: AgentPermissions["opencode"]): OpenCodeDraft {
  const { permission, ...rest } = block ?? {};
  return {
    permission: Object.entries(permission ?? {}).map(([pattern, decision]) => ({
      pattern,
      decision,
    })),
    rest,
  };
}

/** A new agent: Claude, nothing to start with, no servers, no permissions of its own. */
export function emptyDraft(): AgentDraft {
  return {
    name: "",
    description: "",
    runtime: "claude",
    args: "",
    launch: "foreground",
    mcpServers: [],
    mcpBridgeTools: null,
    startup: [],
    defaultPrompt: "",
    role: null,
    resumeAfterRestart: false,
    claude: claudeDraft(null),
    codex: codexDraft(null),
    opencode: opencodeDraft(null),
    hermes: null,
  };
}

/** The draft an agent opens with. */
export function draftFromDefinition(def: AgentDefinition): AgentDraft {
  return {
    name: def.name,
    role: def.role ?? null,
    description: def.description ?? "",
    runtime: def.runtime,
    args: joinArgs(def.args ?? []),
    ...(def.startup_settings !== undefined ? { startupSettings: def.startup_settings } : {}),
    launch: def.launch ?? "foreground",
    mcpServers: def.mcp?.servers ?? [],
    mcpBridgeTools: def.mcp?.bridge_tools ?? null,
    startup: def.startup ?? [],
    defaultPrompt: def.default_prompt ?? "",
    resumeAfterRestart: def.resumeAfterRestart ?? false,
    claude: claudeDraft(def.permissions?.claude),
    codex: codexDraft(def.permissions?.codex),
    opencode: opencodeDraft(def.permissions?.opencode),
    hermes: def.permissions?.hermes ?? null,
  };
}

const nonEmpty = <T extends object>(block: T): T | null =>
  Object.keys(block).length > 0 ? block : null;

function claudeBlock(d: ClaudeDraft): ClaudeBlock | null {
  return nonEmpty({
    ...d.rest,
    ...(d.allow.length ? { allow: d.allow } : {}),
    ...(d.ask.length ? { ask: d.ask } : {}),
    ...(d.deny.length ? { deny: d.deny } : {}),
    ...(d.defaultMode ? { default_mode: d.defaultMode } : {}),
  });
}

function codexBlock(d: CodexDraft): CodexBlock | null {
  return nonEmpty({
    ...d.rest,
    ...(d.approvalPolicy ? { approval_policy: d.approvalPolicy } : {}),
    ...(d.sandboxMode ? { sandbox_mode: d.sandboxMode } : {}),
  });
}

function opencodeBlock(d: OpenCodeDraft): OpenCodeBlock | null {
  const permission = Object.fromEntries(
    d.permission
      .filter(({ pattern, decision }) => pattern.trim() && decision)
      .map(({ pattern, decision }) => [pattern.trim(), decision]),
  );
  return nonEmpty({ ...d.rest, ...(Object.keys(permission).length ? { permission } : {}) });
}

/** The permissions a draft saves: every retained runtime block, or none. */
export function draftPermissions(draft: AgentDraft): AgentPermissions | null {
  const claude = claudeBlock(draft.claude);
  const codex = codexBlock(draft.codex);
  const opencode = opencodeBlock(draft.opencode);
  const hermes = draft.hermes;
  if (!claude && !codex && !opencode && !hermes) return null;
  return {
    ...(claude ? { claude } : {}),
    ...(codex ? { codex } : {}),
    ...(opencode ? { opencode } : {}),
    ...(hermes ? { hermes } : {}),
  };
}

/** What `agent_save` and `agent_preview` receive for `draft`: every field, so an edit replaces
 *  the whole definition. `key` makes it an edit of that agent; else `placement` says where a
 *  new one goes. */
export function formFromDraft(
  draft: AgentDraft,
  target: { key: string } | { placement: AgentPlacement },
): AgentForm {
  const where: Partial<AgentForm> =
    "key" in target
      ? { key: target.key }
      : target.placement.scope === "team"
        ? { scope: "team", org: target.placement.org, team: target.placement.team }
        : { scope: "personal" };
  return {
    ...where,
    role: draft.role,
    name: draft.name.trim(),
    description: draft.description.trim(),
    runtime: draft.runtime,
    args: splitArgs(draft.args),
    ...(draft.startupSettings !== undefined ? { startupSettings: draft.startupSettings } : {}),
    launch: draft.launch,
    mcpServers: draft.mcpServers,
    mcpBridgeTools: draft.mcpBridgeTools,
    startup: draft.startup,
    permissions: draftPermissions(draft),
    defaultPrompt: draft.defaultPrompt,
    resumeAfterRestart: draft.resumeAfterRestart,
  };
}

/** Whether `draft` is a change from `base`, judged on what a save would send. */
export function isChanged(base: AgentDraft, draft: AgentDraft): boolean {
  const target = { key: "" };
  return (
    JSON.stringify(formFromDraft(base, target)) !== JSON.stringify(formFromDraft(draft, target))
  );
}

/** The Edit sections a field belongs to, in the order the editor lists them. */
export type EditorSection =
  | "basics"
  | "runs-on"
  | "tools"
  | "starts-with"
  | "context"
  | "permissions";

const SECTION_OF: Record<keyof AgentDraft, EditorSection> = {
  name: "basics",
  description: "basics",
  defaultPrompt: "basics",
  role: "basics",
  runtime: "runs-on",
  args: "runs-on",
  startupSettings: "runs-on",
  launch: "runs-on",
  resumeAfterRestart: "runs-on",
  mcpServers: "tools",
  mcpBridgeTools: "tools",
  startup: "starts-with",
  claude: "permissions",
  codex: "permissions",
  opencode: "permissions",
  hermes: "permissions",
};

/** The section of Edit a draft field is changed in. */
export function editorSectionOf(field: keyof AgentDraft): EditorSection {
  return SECTION_OF[field];
}

/** The fields of `draft` that differ from `base`, each once (a permission block is one field,
 *  however many of its rules changed). */
export function changedFields(base: AgentDraft, draft: AgentDraft): (keyof AgentDraft)[] {
  return (Object.keys(SECTION_OF) as (keyof AgentDraft)[]).filter(
    (field) => JSON.stringify(base[field]) !== JSON.stringify(draft[field]),
  );
}

/** Top-level content directories no startup path may be under (VCS metadata and dependency
 *  or build trees), as the backend's path rules refuse them. */
const RESERVED_TOP = new Set([".git", ".hg", ".svn", "node_modules", "target"]);

/** A typed startup path checked by the same rules a save applies: its normalized form, or why
 *  it is refused. Whether it exists is the preview's to say. */
export function checkStartupPath(raw: string): { path: string } | { error: string } {
  const shown = JSON.stringify(raw);
  if (raw.includes("\0")) return { error: `startup path ${shown} contains a NUL byte` };
  if (/^[/\\]/.test(raw) || /^[A-Za-z]:/.test(raw)) {
    return { error: `startup path ${shown} is absolute; use a path relative to the content root` };
  }
  if (/[*?[\]]/.test(raw)) {
    return {
      error: `startup path ${shown} contains a glob character; name a file or directory instead`,
    };
  }
  if (raw.includes("\\")) {
    return { error: `startup path ${shown} contains a backslash; separate segments with \`/\`` };
  }
  const segments = raw.split("/").filter((s) => s !== "" && s !== ".");
  if (segments.includes("..")) {
    return { error: `startup path ${shown} may not contain a \`..\` segment` };
  }
  if (segments.length === 0) return { error: `startup path ${shown} is empty` };
  if (RESERVED_TOP.has(segments[0].toLowerCase())) {
    return {
      error: `startup path ${shown} is under the reserved directory ${JSON.stringify(segments[0])}`,
    };
  }
  return { path: segments.join("/") };
}

// ---- SW-XS-16: context entries, checked by the rules a save applies --------------------
// (`agents/context.rs`: `normalize_entries`, `unsafe_destination`). Whether a prefix exists,
// and dest collisions across entries, are the server's to say.

/** The most entries a `context` list may hold (`MAX_CONTEXT_ENTRIES`). */
export const MAX_CONTEXT_ENTRIES = 32;

/** The server's own lists (`agents/context.rs`, codesense-fs-core `filters.rs`), emitted
 *  into this fixture by `context::tests::filter_globs_fixture_is_current`, so a dest the
 *  editor lets through is one a save accepts. */
const RULES = FILTER_GLOBS as {
  vcs_dirs: string[];
  managed_destinations: string[];
  junk: string[];
  ignore: string[];
  secret: string[];
  runtime_state: string[];
  case_insensitive: string[];
};

type GlobList = "junk" | "ignore" | "secret" | "runtime_state";

/** Version-control metadata: never a context path or a dest, at any depth, in any case. */
const VCS_DIRS = new Set(RULES.vcs_dirs);

/** Workspace files CodeSense or the runtime CLI owns (`MANAGED_DESTINATIONS`). */
const MANAGED_DESTINATIONS = RULES.managed_destinations;

/** The server's matcher for these lists: case-sensitive (globset's default) unless the
 *  fixture names a list as matched in any case (SW-XS-12: secrets, VCS, cruft and runtime
 *  state, since on macOS `config/.ENV` is `config/.env`). */
function globsetMatcher(lists: readonly GlobList[]): (path: string) => boolean {
  const matchers = lists.flatMap((list) =>
    RULES[list].map((g) => globMatcherWithCase(g, !RULES.case_insensitive.includes(list))),
  );
  return (path) => matchers.some((m) => m(path));
}

/** `JUNK_GLOBS`: exact local-state and tool-lock files context never carries. */
const isJunk = globsetMatcher(["junk"]);
/** `IGNORE_GLOBS`: OS, editor, build and VCS cruft a workspace never carries. */
const isIgnored = globsetMatcher(["ignore"]);
/** `TeamExcludes::default_only`: `SECRET_GLOBS` plus runtime state. A team's own
 *  `projection-excludes.yml` is the server's to apply. */
const isDefaultExcluded = globsetMatcher(["secret", "runtime_state"]);

/** `not_carried`: ignored, a directory whose whole subtree is ignored, or VCS data. */
function notCarried(path: string): boolean {
  return (
    isIgnored(path) ||
    isIgnored(`${path.replace(/\/+$/, "")}/__codesense_walk_probe__`) ||
    vcsSegment(path) !== undefined
  );
}

/** `raw` by the startup-path rules, NFC first, refusals worded for `what`. */
function checkContentPath(raw: string, what: string): { path: string } | { error: string } {
  const checked = checkStartupPath(raw.normalize("NFC"));
  if ("error" in checked) return { error: checked.error.replace(/^startup path /, `${what} `) };
  return checked;
}

function vcsSegment(path: string): string | undefined {
  return path.split("/").find((s) => VCS_DIRS.has(s.toLowerCase()));
}

/** A typed context path, normalized as a save normalizes it, or why it is refused. */
export function checkContextPath(raw: string): { path: string } | { error: string } {
  const checked = checkContentPath(raw, "context path");
  if ("error" in checked) return checked;
  const vcs = vcsSegment(checked.path);
  if (vcs) {
    return { error: `context path ${JSON.stringify(raw)}: ${vcs} is version-control metadata` };
  }
  return checked;
}

/** A typed `dest`: `null` when blank (no remap), else normalized, or why it is refused. */
export function checkContextDest(raw: string): { dest: string | null } | { error: string } {
  if (!raw.trim()) return { dest: null };
  const checked = checkContentPath(raw.trim(), "dest");
  if ("error" in checked) return checked;
  const dest = checked.path;
  const shown = JSON.stringify(dest);
  const vcs = vcsSegment(dest);
  if (vcs)
    return { error: `dest ${shown} cannot receive context: ${vcs} is version-control metadata` };
  if (notCarried(dest)) {
    return {
      error: `dest ${shown} cannot receive context: it is OS, editor, build or VCS data a workspace never carries`,
    };
  }
  const folded = dest.toLowerCase();
  const managed = MANAGED_DESTINATIONS.find((m) => folded === m || folded.startsWith(`${m}/`));
  if (managed) {
    return {
      error: `dest ${shown} cannot receive context: ${managed} is generated or managed by CodeSense`,
    };
  }
  if (isJunk(dest) || isDefaultExcluded(dest)) {
    return {
      error: `dest ${shown} cannot receive context: it is never copied into a workspace by default`,
    };
  }
  return { dest };
}

/** Whether `a` lies inside `b`, segment-wise, or is `b`. */
function within(a: string, b: string): boolean {
  return a === b || a.startsWith(`${b}/`);
}

/** Why `path` cannot join `startup` (already listed, or inside or around a listed entry), or
 *  `null` when it can. */
export function startupConflict(startup: readonly string[], path: string): string | null {
  for (const existing of startup) {
    if (existing === path) return `${path} is already listed`;
    if (within(path, existing) || within(existing, path)) {
      return `${path} overlaps ${existing}; list one of them`;
    }
  }
  return null;
}

/** The path as the list shows it: a folder with a trailing `/`. */
export function displayPath(path: string, isDir: boolean): string {
  return isDir && !path.endsWith("/") ? `${path}/` : path;
}

/** One field a revision changed. */
export interface FieldChange {
  field: string;
  before: string;
  after: string;
}

const list = (items: readonly string[] | undefined) => (items ?? []).join(", ");

/** A definition's env as `NAME=value` pairs in name order, as it is stored. */
const envText = (env: Record<string, string> | undefined) =>
  Object.entries(env ?? {})
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([name, value]) => `${name}=${value}`)
    .join(", ");

function permissionsText(def: AgentDefinition, runtime: AgentRuntime): string {
  if (runtime === "claude") {
    const c = def.permissions?.claude;
    if (!c) return "";
    const parts = [];
    if (c.allow?.length) parts.push(`allow ${list(c.allow)}`);
    if (c.ask?.length) parts.push(`ask ${list(c.ask)}`);
    if (c.deny?.length) parts.push(`deny ${list(c.deny)}`);
    if (c.default_mode) parts.push(`mode ${c.default_mode}`);
    return parts.join("; ");
  }
  if (runtime === "opencode") {
    const rules = def.permissions?.opencode?.permission ?? {};
    return Object.entries(rules)
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([pattern, decision]) => `${pattern} ${decision}`)
      .join("; ");
  }
  const c = def.permissions?.codex;
  if (!c) return "";
  return [
    c.approval_policy && `approval ${c.approval_policy}`,
    c.sandbox_mode && `sandbox ${c.sandbox_mode}`,
  ]
    .filter(Boolean)
    .join("; ");
}

/** What `next` changed against `prev`, field by field in the sentence's order. Startup paths
 *  are compared as a set and reported as what was added and removed. */
export function revisionDiff(prev: AgentDefinition, next: AgentDefinition): FieldChange[] {
  const fields: [string, (d: AgentDefinition) => string][] = [
    ["Name", (d) => d.name],
    ["Description", (d) => d.description ?? ""],
    ["Runtime", (d) => d.runtime],
    ["Args", (d) => joinArgs(d.args ?? [])],
    ["Env", (d) => envText(d.env)],
    ["Launch", (d) => d.launch ?? "foreground"],
    ["MCP servers", (d) => list(d.mcp?.servers)],
    ["Claude permissions", (d) => permissionsText(d, "claude")],
    ["Codex permissions", (d) => permissionsText(d, "codex")],
    ["OpenCode permissions", (d) => permissionsText(d, "opencode")],
    ["Default role", (d) => d.role ?? ""],
    ["Default prompt", (d) => d.default_prompt ?? ""],
    ["Archived", (d) => (d.archived_at ? "archived" : "")],
  ];
  const changes: FieldChange[] = [];
  for (const [field, read] of fields) {
    const before = read(prev);
    const after = read(next);
    if (before !== after) changes.push({ field, before, after });
    if (field === "MCP servers") {
      const was = new Set(prev.startup ?? []);
      const now = new Set(next.startup ?? []);
      const added = [...now].filter((p) => !was.has(p));
      const removed = [...was].filter((p) => !now.has(p));
      const reordered =
        added.length === 0 && removed.length === 0 && list(prev.startup) !== list(next.startup);
      if (added.length || removed.length || reordered) {
        changes.push({
          field: "Startup",
          before: removed.length ? `removed ${list(removed)}` : reordered ? list(prev.startup) : "",
          after: added.length ? `added ${list(added)}` : reordered ? list(next.startup) : "",
        });
      }
    }
  }
  return changes;
}
