// SPDX-License-Identifier: MIT
import { escapeGlob, globMatcher, isUnder } from "@/features/agent-context/lib/agent-context-model";
import type {
  AgentContextEditReply,
  AgentContextEntry,
  AgentContextFile,
  AgentContextRoot,
  AgentContextView,
  AgentRepoOverride,
  AgentRuntime,
} from "@/tauri/commands";

/**
 * The mock backend's agent context (SW-XS-15 nav world, SW-XS-16 agent editor): the want set
 * `(context | runtime default) + startup + override.add − override.remove`, the repo tier
 * winning per file, and the server-side edits of `agents/context_edit.rs`, simplified. One
 * implementation, so the nav overlay and the editor's Context section agree in mock mode.
 */

/** An agent as the context mock holds it. */
export interface MockContextAgent {
  key: string;
  name: string;
  runtime: AgentRuntime;
  revision: number;
  context: AgentContextEntry[] | null;
  startup: string[];
  overrides: AgentRepoOverride[];
}

/** Where want-set files come from. */
export interface MockContextTiers {
  org: string;
  team: string;
  teamRoot: string;
  teamFiles: Record<string, number>;
  /** A repo's own tier (`host/owner/repo`), if it has one. */
  repoTier: (repo: string) => { root: string; files: Record<string, number> } | null;
  /** Multiply the totals so a few demo files read like a real team's want set. */
  scale?: { files: number; bytes: number };
}

const RUNTIME_DEFAULT: Record<AgentRuntime, string[]> = {
  claude: ["CLAUDE.md", ".claude/commands", ".claude/hooks", ".claude/agents", ".claude/skills"],
  codex: ["AGENTS.md", ".agents/skills"],
  hermes: ["AGENTS.md", ".agents/skills"],
  opencode: ["AGENTS.md", ".opencode/command", ".opencode/agent", ".agents/skills"],
};
const TEAM_WORKSPACE_DEFAULT = [".claude/commands", ".claude/skills"];
const JUNK = new Set([".claude/settings.local.json", ".claude/scheduled_tasks.lock"]);
const LARGE_BYTES = 50 * 1024 * 1024;
const LARGE_FILES = 5_000;

export function mockRuntimeDefault(runtime: AgentRuntime): AgentContextEntry[] {
  return RUNTIME_DEFAULT[runtime].map((path) => ({ path }));
}

function overrideFor(a: MockContextAgent, repo: string | null): AgentRepoOverride | undefined {
  if (!repo) return undefined;
  return a.overrides.find((o) => o.repo.toLowerCase() === repo.toLowerCase());
}

/** Whether `entry` delivers `other`'s prefix where `other` would land it (Rust
 *  `ContextEntry::places_like`). */
export function placesLike(entry: AgentContextEntry, other: AgentContextEntry): boolean {
  return (
    isUnder(other.path, entry.path) &&
    destinationOf(entry, other.path) === (other.dest ?? other.path)
  );
}

/** Where `logical` lands under `entry`. */
function destinationOf(entry: { path: string; dest?: string }, logical: string): string {
  if (!entry.dest) return logical;
  return entry.dest + logical.slice(entry.path.length);
}

/** `a`'s want set for `repo` (none: a team-workspace launch's). */
export function resolveMockContext(
  tiers: MockContextTiers,
  a: MockContextAgent,
  repo: string | null,
  suggestion: AgentContextView["context_suggestion"] = null,
): AgentContextView {
  const defaults = repo ? RUNTIME_DEFAULT[a.runtime] : TEAM_WORKSPACE_DEFAULT;
  type Want = {
    path: string;
    dest?: string;
    source: AgentContextView["entries"][number]["source"];
  };
  const want: Want[] = [
    ...a.startup.map((path) => ({ path, source: "startup" as const })),
    ...(a.context?.map((e) => ({ ...e, source: "context" as const })) ??
      defaults.map((path) => ({ path, source: "default" as const }))),
  ];
  const override = overrideFor(a, repo);
  for (const e of override?.add ?? []) want.push({ ...e, source: "repo_add" });
  const removals = (override?.remove ?? []).map(globMatcher);
  const removed = (rel: string) => {
    if (a.startup.some((s) => isUnder(rel, s))) return false;
    const segments = rel.split("/");
    return segments.some((_, i) => removals.some((m) => m(segments.slice(0, i + 1).join("/"))));
  };
  const repoTier = repo ? tiers.repoTier(repo) : null;
  const roots: AgentContextRoot[] = [
    ...(repoTier && repo
      ? [
          {
            tier: { kind: "repo" as const, org: tiers.org, team: tiers.team, repo },
            path: repoTier.root,
          },
        ]
      : []),
    { tier: { kind: "team", org: tiers.org, team: tiers.team }, path: tiers.teamRoot },
  ];
  const sources: Array<[Record<string, number>, number]> = repoTier
    ? [
        [repoTier.files, 0],
        [tiers.teamFiles, 1],
      ]
    : [[tiers.teamFiles, 0]];
  const files = new Map<string, AgentContextFile>();
  const entries: AgentContextView["entries"] = [];
  const warnings: AgentContextView["warnings"] = [];
  for (const w of want) {
    let n = 0;
    let bytes = 0;
    let present = false;
    for (const [content, root] of sources) {
      for (const [rel, size] of Object.entries(content)) {
        if (!isUnder(rel, w.path)) continue;
        present = true;
        if (JUNK.has(rel) || removed(rel)) continue;
        const destination = destinationOf(w, rel);
        if (files.has(destination)) continue;
        files.set(destination, { destination, logical: rel, bytes: size, root });
        n += 1;
        bytes += size;
      }
    }
    entries.push({
      path: w.path,
      ...(w.dest ? { dest: w.dest } : {}),
      source: w.source,
      files: n,
      bytes,
    });
    if (w.source === "default" || w.source === "startup") continue;
    if (JUNK.has(w.path)) {
      warnings.push({
        path: w.path,
        code: "excluded",
        message: `${w.path} is in the team's excluded paths, so it is never copied into a workspace`,
      });
    } else if (!present) {
      warnings.push({ path: w.path, code: "missing", message: `${w.path} is in no content tier` });
    }
  }
  const list = [...files.values()].sort((x, y) => x.destination.localeCompare(y.destination));
  const raw = { files: list.length, bytes: list.reduce((s, f) => s + f.bytes, 0) };
  const totals = tiers.scale
    ? { files: raw.files * tiers.scale.files, bytes: raw.bytes * tiers.scale.bytes }
    : raw;
  if (totals.bytes > LARGE_BYTES || totals.files > LARGE_FILES) {
    warnings.push({
      code: "large",
      message: `the want set is unusually large: ${totals.files} files, ${(totals.bytes / (1024 * 1024)).toFixed(1)} MB (hint: ${LARGE_FILES} files or ${LARGE_BYTES / (1024 * 1024)} MB)`,
    });
  }
  return {
    key: a.key,
    name: a.name,
    revision: a.revision,
    context: a.context,
    runtime_default: mockRuntimeDefault(a.runtime),
    startup: a.startup,
    repo_overrides: a.overrides,
    repo,
    defaulted: a.context === null,
    entries,
    roots,
    files: list,
    truncated: false,
    totals,
    warnings,
    context_suggestion: suggestion,
  };
}

/** One `agent_context_update` operation, as the command takes it. */
export interface MockContextEdit {
  op: string;
  path?: string | null;
  dest?: string | null;
  entries?: AgentContextEntry[] | null;
  repo?: string | null;
}

/** `list` after removing `path` (Rust `after_remove`). */
function afterRemove(list: AgentContextEntry[], path: string): AgentContextEntry[] {
  const exact = list.find((e) => e.path === path);
  return exact
    ? list.filter((e) => e.path !== path && !placesLike(exact, e))
    : list.filter((e) => !isUnder(e.path, path));
}

/** Apply one edit to `a` (not its revision): `null` when made, else why it cannot be. */
export function applyMockContextEdit(a: MockContextAgent, edit: MockContextEdit): string | null {
  const path = (edit.path ?? "").trim().replace(/\/+$/, "").replace(/^\.\//, "");
  const dest = edit.dest?.trim() ? edit.dest.trim().replace(/\/+$/, "") : undefined;
  const entry = (p: string): AgentContextEntry => (dest ? { path: p, dest } : { path: p });
  const base = a.context ?? mockRuntimeDefault(a.runtime);
  const covering = (p: string, list: AgentContextEntry[]) => list.find((e) => isUnder(p, e.path));
  if (edit.op === "customize") {
    if (a.context) return `${a.name}'s context is already its own list.`;
    a.context = base;
    return null;
  }
  if (edit.op === "add_entries") {
    let next = [...base];
    let added = 0;
    for (const e of edit.entries ?? []) {
      const entry: AgentContextEntry = { ...e };
      if (next.some((x) => placesLike(x, entry))) continue;
      next = [...next.filter((x) => !placesLike(entry, x)), entry];
      added += 1;
    }
    if (added === 0) return `Already in ${a.name}'s context.`;
    a.context = next;
    return null;
  }
  if (!path) return "Pick a file or folder.";
  if (!edit.repo) {
    if (edit.op === "add") {
      const added = entry(path);
      // Coverage is by place (Rust `plan` / `plan_add`).
      const c = base.find((e) => placesLike(e, added));
      if (c) return `Already included through ${c.path}.`;
      const same = base.find((e) => e.path === path);
      if (same) return `${path} already lands at ${same.dest ?? path}. Remove it first to move it.`;
      a.context = [...base.filter((e) => !placesLike(added, e)), added];
      return null;
    }
    if (edit.op === "remove") {
      const next = afterRemove(base, path);
      if (next.length === base.length) return `Not in ${a.name}'s own context.`;
      a.context = next;
      return null;
    }
    return `Unknown edit ${edit.op}.`;
  }
  const repo = edit.repo;
  const o = overrideFor(a, repo) ?? { repo, add: [], remove: [] };
  const next: AgentRepoOverride = { ...o, add: [...o.add], remove: [...o.remove] };
  const escaped = escapeGlob(path);
  if (edit.op === "add") {
    const before = next.remove.length;
    next.remove = next.remove.filter((g) => !(g === escaped || isUnder(g, escaped)));
    const added = entry(path);
    const covered = [...base, ...next.add].some((e) => placesLike(e, added));
    if (!covered) next.add = [...next.add.filter((e) => !placesLike(added, e)), added];
    else if (before === next.remove.length) return `Already in ${a.name}'s context for this repo.`;
  } else if (edit.op === "remove") {
    const before = next.add.length;
    next.add = afterRemove(next.add, path);
    if (before === next.add.length) {
      if (!covering(path, base)) return `Not in ${a.name}'s context for this repo.`;
      next.remove = [...next.remove.filter((g) => !isUnder(g, escaped)), escaped];
    }
  } else if (edit.op === "drop_add") {
    const before = next.add.length;
    next.add = next.add.filter((e) => e.path !== path);
    if (before === next.add.length) return `${a.name} does not add ${path} for this repo.`;
  } else if (edit.op === "drop_remove") {
    const bare = (g: string) => g.replace(/\/+$/, "");
    if (!next.remove.some((g) => bare(g) === path)) {
      return `${path} is not left out for this repo.`;
    }
    next.remove = next.remove.filter((g) => bare(g) !== path);
  } else {
    return `Unknown edit ${edit.op}.`;
  }
  a.overrides = [...a.overrides.filter((x) => x.repo.toLowerCase() !== repo.toLowerCase()), next];
  return null;
}

/** The whole `agent_context_update` reply for `a`, given the command's args. */
export function mockContextUpdate(
  tiers: MockContextTiers,
  a: MockContextAgent,
  args: Record<string, unknown>,
  onSaved: (accepted: string | null) => void,
  suggestion: (a: MockContextAgent) => AgentContextView["context_suggestion"] = () => null,
): AgentContextEditReply {
  if (Number(args.baseRevision) !== a.revision) {
    return { outcome: "conflict", current_revision: a.revision };
  }
  const blocked = applyMockContextEdit(a, {
    op: String(args.op),
    path: args.path as string | null,
    dest: args.dest as string | null,
    // As the command takes them: `{path, dest?, keep?}`, `keep` the entry's other keys.
    entries: (
      args.entries as Array<AgentContextEntry & { keep?: Record<string, unknown> }> | null
    )?.map(({ keep, ...e }) => ({ ...keep, ...e })),
    repo: args.repo as string | null,
  });
  if (blocked) return { outcome: "blocked", reason: blocked };
  a.revision += 1;
  onSaved(args.op === "add_entries" && args.accepts ? String(args.accepts) : null);
  const viewRepo = args.viewRepo ? String(args.viewRepo) : null;
  return {
    outcome: "saved",
    key: a.key,
    revision: a.revision,
    view: resolveMockContext(tiers, a, viewRepo, suggestion(a)),
  };
}
