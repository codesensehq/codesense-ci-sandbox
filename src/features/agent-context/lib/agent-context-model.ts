// SPDX-License-Identifier: MIT
import type { AgentContextEntry, AgentContextTier, AgentContextView } from "@/tauri/commands";
import type { SourceRepoSlug } from "@/shared/types/domain";

/**
 * SW-XS-15 — the left nav's context pivot maps to an AGENT.
 *
 * The pivot answers "what does this agent get here?": either **Team (everything)** (the raw
 * context repo, implying nothing about delivery) or **Context for ‹Agent›**, optionally
 * **· in ‹Repo›**. This module is the pure model behind it: the persisted selection, the
 * header words, which nav paths are in the agent's want set (and from which tier), and a
 * PREDICTION of what a right-click "Add to / Remove from ‹Agent›'s context" will do, so the
 * menu can say why an edit is unavailable. The server applies the edit itself
 * (`agent_context_update`, `agents/context_edit.rs`); these rules mirror it.
 */

/** The persisted per-team selection. Absent = Team (everything). */
export interface AgentContextSelection {
  /** The agent's key, as `agents_list` returns it. */
  agentKey: string;
  /** `host/owner/repo` of one of the team's source repos (older saves: `owner/repo`, which
   *  means github.com); absent = the team workspace. */
  repoSlug?: string;
}

/** The pivot's first option: the raw context repo. */
export const TEAM_EVERYTHING_LABEL = "Team (everything)";

/** "Context for Barb" / "Context for Barb · in codesense". */
export function contextHeaderLabel(agentName: string, repoName?: string | null): string {
  return repoName ? `Context for ${agentName} · in ${repoName}` : `Context for ${agentName}`;
}

/** A byte count in the units a person reads (one decimal from KB up). */
export function formatContextBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/** "1,214 files · 18.2 MB". */
export function contextSummary(totals: { files: number; bytes: number }): string {
  const files = `${totals.files.toLocaleString("en-US")} ${totals.files === 1 ? "file" : "files"}`;
  return `${files} · ${formatContextBytes(totals.bytes)}`;
}

/** The `repo` an agent call takes, and the pivot persists, for a team source repo:
 *  `host/owner/repo`. */
export function repoRequestLabel(slug: Pick<SourceRepoSlug, "owner" | "repo" | "host">): string {
  return `${slug.host || "github.com"}/${slug.owner}/${slug.repo}`;
}

/** Does a persisted `repoSlug` name this repo? (`owner/repo` from older saves = github.com.) */
export function repoSlugMatches(
  persisted: string,
  slug: Pick<SourceRepoSlug, "owner" | "repo" | "host">,
): boolean {
  const full = persisted.split("/").length === 2 ? `github.com/${persisted}` : persisted;
  return full.toLowerCase() === repoRequestLabel(slug).toLowerCase();
}

/** The quiet source tag a nav row carries. */
export type ContextTierTag = "team" | "repo" | "personal";

export function tierTag(tier: AgentContextTier): ContextTierTag {
  return tier.kind;
}

/** Whether a nav path is in the agent's want set: wholly (`in`), some of what is under a
 *  folder (`partial`), or not at all. */
export interface ContextMembership {
  state: "in" | "partial" | "out";
  /** The tier the path (or everything in the folder) comes from; absent when mixed or
   *  unknown. */
  tier?: ContextTierTag;
  /** `in` with several tiers under it. */
  mixed?: boolean;
}

const OUT: ContextMembership = { state: "out" };

function ancestorsOf(rel: string): string[] {
  const out: string[] = [];
  let i = rel.indexOf("/");
  while (i !== -1) {
    out.push(rel.slice(0, i));
    i = rel.indexOf("/", i + 1);
  }
  return out;
}

/** Is `rel` the entry `path` or inside it? */
export function isUnder(rel: string, path: string): boolean {
  return rel === path || rel.startsWith(`${path}/`);
}

// ---------------------------------------------------------------------------
// Globs — the resolver's `remove` matcher (globset, `literal_separator`, case-insensitive on
// macOS/Windows): `*` and `?` never cross `/`, `**` spans folders, `[…]` / `[!…]` classes,
// `{a,b}` alternatives.
// ---------------------------------------------------------------------------

const REGEX_SPECIAL = /[.+^$()|\\/]/;

function globBody(glob: string): string {
  let out = "";
  let i = 0;
  while (i < glob.length) {
    const c = glob[i];
    if (c === "*") {
      if (glob[i + 1] === "*") {
        const atStart = i === 0 || glob[i - 1] === "/";
        const next = glob[i + 2];
        if (atStart && next === "/") {
          out += "(?:.*/)?";
          i += 3;
          continue;
        }
        if (atStart && next === undefined) {
          out += ".*";
          i += 2;
          continue;
        }
      }
      out += "[^/]*";
      i += 1;
    } else if (c === "?") {
      out += "[^/]";
      i += 1;
    } else if (c === "[") {
      let j = i + 1;
      let negate = false;
      if (glob[j] === "!" || glob[j] === "^") {
        negate = true;
        j += 1;
      }
      let cls = "";
      // A `]` right after the opening is literal.
      if (glob[j] === "]") {
        cls += "\\]";
        j += 1;
      }
      while (j < glob.length && glob[j] !== "]") {
        const ch = glob[j];
        cls += ch === "\\" || ch === "^" || ch === "[" ? `\\${ch}` : ch;
        j += 1;
      }
      if (j >= glob.length) {
        out += "\\[";
        i += 1;
        continue;
      }
      out += `[${negate ? "^" : ""}${cls}]`;
      i = j + 1;
    } else if (c === "{") {
      const close = glob.indexOf("}", i);
      if (close === -1) {
        out += "\\{";
        i += 1;
        continue;
      }
      const alts = glob.slice(i + 1, close).split(",");
      out += `(?:${alts.map(globBody).join("|")})`;
      i = close + 1;
    } else {
      out += REGEX_SPECIAL.test(c) || c === "}" || c === "]" ? `\\${c}` : c;
      i += 1;
    }
  }
  return out;
}

/** A `remove` glob as a matcher over content paths, ignoring case (as on macOS/Windows). */
export function globMatcher(glob: string): (path: string) => boolean {
  return globMatcherWithCase(glob, false);
}

/** `globMatcher`, matching case exactly when `caseSensitive` (globset's default, which the
 *  projection filters use). */
export function globMatcherWithCase(
  glob: string,
  caseSensitive: boolean,
): (path: string) => boolean {
  let re: RegExp;
  try {
    re = new RegExp(`^${globBody(glob)}$`, caseSensitive ? "" : "i");
  } catch {
    return () => false;
  }
  return (path) => re.test(path);
}

/** A literal path as a `remove` glob (as the server writes it). */
export function escapeGlob(path: string): string {
  return path.replace(/[*?[\]{}]/g, (c) => `[${c}]`);
}

/** Is `rel` (or a folder above it) removed by any of these globs? */
function removedBy(globs: ReadonlyArray<(p: string) => boolean>, rel: string): boolean {
  if (globs.length === 0) return false;
  return [rel, ...ancestorsOf(rel)].some((p) => globs.some((g) => g(p)));
}

function overrideOf(view: Pick<AgentContextView, "repo" | "repo_overrides">) {
  if (!view.repo) return null;
  const repo = view.repo.toLowerCase();
  return view.repo_overrides.find((o) => o.repo.toLowerCase() === repo) ?? null;
}

/**
 * Index the want set by content-relative path. Each file is keyed by its `logical` path (where
 * it sits in its tier, so where the nav shows it), and each folder above it collects the tiers
 * its files come from. A folder an entry covers whole is `in`; one with only some files in the
 * set is `partial`. When the file list was cut short, a path an entry covers (and the repo's
 * removals do not drop) still reads as `in`, its tier unknown.
 */
export function buildContextMembership(
  view: Pick<
    AgentContextView,
    "files" | "truncated" | "entries" | "roots" | "startup" | "repo" | "repo_overrides"
  >,
): (rel: string) => ContextMembership {
  const files = new Map<string, ContextTierTag | undefined>();
  const dirs = new Map<string, Set<ContextTierTag | "?">>();
  for (const f of view.files) {
    const root = f.root === null ? undefined : view.roots[f.root];
    const tag = f.tier ? tierTag(f.tier) : root ? tierTag(root.tier) : undefined;
    if (!files.has(f.logical)) files.set(f.logical, tag);
    for (const dir of ancestorsOf(f.logical)) {
      let set = dirs.get(dir);
      if (!set) dirs.set(dir, (set = new Set()));
      set.add(tag ?? "?");
    }
  }
  const startup = view.startup.map((s) => s.replace(/\/+$/, ""));
  const removePatterns = overrideOf(view)?.remove ?? [];
  const removals = removePatterns.map(globMatcher);
  /** Does a removal reach strictly inside `rel` (a covered folder that lost a subfolder)? */
  const removedInside = (rel: string) => {
    const prefix = `${escapeGlob(rel)}/`;
    return removePatterns.some((g) => g.toLowerCase().startsWith(prefix.toLowerCase()));
  };
  const covered = (rel: string) =>
    startup.some((s) => isUnder(rel, s)) ||
    (view.entries.some((e) => isUnder(rel, e.path)) && !removedBy(removals, rel));
  return (rel: string) => {
    if (rel === "") return OUT;
    if (files.has(rel)) {
      const tier = files.get(rel);
      return tier ? { state: "in", tier } : { state: "in" };
    }
    const tiers = dirs.get(rel);
    if (tiers) {
      const known = [...tiers].filter((t): t is ContextTierTag => t !== "?");
      const single = tiers.size === 1 && known.length === 1 ? known[0] : undefined;
      if (!covered(rel) || removedInside(rel)) return { state: "partial" };
      return single ? { state: "in", tier: single } : { state: "in", mixed: tiers.size > 1 };
    }
    if (view.truncated && covered(rel)) {
      return removedInside(rel) ? { state: "partial" } : { state: "in" };
    }
    return OUT;
  };
}

// ---------------------------------------------------------------------------
// Edit prediction (mirrors `agents/context_edit.rs`).
// ---------------------------------------------------------------------------

export type ContextEditPlan =
  | {
      kind: "edit";
      optimistic: "in" | "out";
      /** A team-wide edit while the runtime default applies: the edit writes ‹Agent›'s
       *  context out explicitly, for every launch. */
      makesExplicit: boolean;
    }
  | { kind: "blocked"; reason: string };

/** Whether `entry` delivers `other`'s prefix where `other` itself would land it (Rust
 *  `ContextEntry::places_like`): coverage is by place, so a folder above that remaps a
 *  path does not cover an entry for the path. */
export function placesLike(entry: AgentContextEntry, other: AgentContextEntry): boolean {
  if (!isUnder(other.path, entry.path)) return false;
  const lands = entry.dest ? entry.dest + other.path.slice(entry.path.length) : other.path;
  return lands === (other.dest ?? other.path);
}

/** The agent's `context` as an edit starts from it. */
function baseContext(view: AgentContextView): AgentContextEntry[] {
  return view.context ?? view.runtime_default;
}

/** `list` after removing `rel` (Rust `after_remove`): the entry for exactly `rel` and what it
 *  places the same, keeping nested entries that land elsewhere; with no entry for exactly
 *  `rel`, everything inside it. */
function afterRemove(list: AgentContextEntry[], rel: string): AgentContextEntry[] {
  const exact = list.find((e) => e.path === rel);
  return exact
    ? list.filter((e) => e.path !== rel && !placesLike(exact, e))
    : list.filter((e) => !isUnder(e.path, rel));
}

function pinnedBy(view: AgentContextView, rel: string): string | undefined {
  return view.startup.map((s) => s.replace(/\/+$/, "")).find((s) => isUnder(rel, s));
}

function overrideFor(view: AgentContextView, repo: string) {
  const found = view.repo_overrides.find((o) => o.repo.toLowerCase() === repo.toLowerCase());
  return { add: [...(found?.add ?? [])], remove: [...(found?.remove ?? [])] };
}

/** What "Add to ‹Agent›'s context" (team-wide, or with `repo` "… for ‹Repo› only") does. */
export function planAdd(view: AgentContextView, rel: string, repo?: string): ContextEditPlan {
  if (!rel) return { kind: "blocked", reason: "Pick a file or folder." };
  const base = baseContext(view);
  if (repo) {
    const o = overrideFor(view, repo);
    // Only a removal of exactly this path, or of something inside it, is this edit's to
    // undo; a broader pattern that matches it would bring back much more.
    const escaped = escapeGlob(rel);
    const mine = (g: string) => g === escaped || isUnder(g, escaped);
    const broader = o.remove.find((g) => {
      if (mine(g)) return false;
      const m = globMatcher(g);
      return [rel, ...ancestorsOf(rel)].some((a) => m(a));
    });
    if (broader) {
      return {
        kind: "blocked",
        reason: `Removed for this repo through ${broader}; edit that pattern in the agent editor.`,
      };
    }
    const dropsRemoval = o.remove.some(mine);
    const covered = [...base, ...o.add].some((e) => placesLike(e, { path: rel }));
    if (covered && !dropsRemoval) {
      return { kind: "blocked", reason: `Already in ${view.name}'s context for this repo.` };
    }
    return { kind: "edit", optimistic: "in", makesExplicit: false };
  }
  const covering = base.find((e) => placesLike(e, { path: rel }));
  if (covering) {
    return { kind: "blocked", reason: `Already included through ${covering.path}.` };
  }
  return { kind: "edit", optimistic: "in", makesExplicit: view.context === null };
}

/** What "Remove from ‹Agent›'s context" (team-wide, or with `repo` "… for ‹Repo› only") does.
 *  A startup path is never removed (it is pinned), and a path inside a broader entry can be
 *  removed for a repo only, since `context` has no exclusions. */
export function planRemove(view: AgentContextView, rel: string, repo?: string): ContextEditPlan {
  if (!rel) return { kind: "blocked", reason: "Pick a file or folder." };
  const pinned = pinnedBy(view, rel);
  if (pinned) {
    return {
      kind: "blocked",
      reason: `${view.name} starts with ${pinned}. Change its startup files in the agent editor.`,
    };
  }
  const base = baseContext(view);
  if (repo) {
    const o = overrideFor(view, repo);
    const adds = afterRemove(o.add, rel);
    const dropsAdd = adds.length !== o.add.length;
    const still = base.some((e) => isUnder(rel, e.path)) || adds.some((e) => isUnder(rel, e.path));
    const already = removedBy(o.remove.map(globMatcher), rel);
    if (!dropsAdd && (!still || already)) {
      return { kind: "blocked", reason: `Not in ${view.name}'s context for this repo.` };
    }
    return { kind: "edit", optimistic: "out", makesExplicit: false };
  }
  const exact = base.some((e) => e.path === rel);
  const outer = exact ? undefined : base.find((e) => isUnder(rel, e.path) && e.path !== rel);
  if (outer) {
    return {
      kind: "blocked",
      reason: `Included through ${outer.path}. Remove that folder, or pick a repo and remove it for that repo only.`,
    };
  }
  if (!base.some((e) => isUnder(e.path, rel))) {
    return { kind: "blocked", reason: `Not in ${view.name}'s own context.` };
  }
  return { kind: "edit", optimistic: "out", makesExplicit: view.context === null };
}
