// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import type { AgentContextEntry, AgentContextFile, AgentContextView } from "@/tauri/commands";

import {
  buildContextMembership,
  contextHeaderLabel,
  contextSummary,
  escapeGlob,
  globMatcher,
  placesLike,
  planAdd,
  planRemove,
  repoRequestLabel,
  repoSlugMatches,
} from "./agent-context-model";

const CODESENSE = "github.com/alderco/codesense";
const ROOTS: AgentContextView["roots"] = [
  { tier: { kind: "repo", org: "alderco", team: "default", repo: CODESENSE }, path: "/r" },
  { tier: { kind: "team", org: "alderco", team: "default" }, path: "/c" },
];
const REPO = 0;
const TEAM = 1;

function file(logical: string, root: number | null = TEAM): AgentContextFile {
  return { destination: logical, logical, bytes: 10, root };
}

const DEFAULT = [
  { path: "CLAUDE.md" },
  { path: ".claude/commands" },
  { path: ".claude/hooks" },
  { path: ".claude/agents" },
  { path: ".claude/skills" },
];

/** Barb with the claude runtime default (no stored context), resolved in codesense. */
function barb(over: Partial<AgentContextView> = {}): AgentContextView {
  return {
    key: "alderco/default/barb",
    name: "Barb",
    revision: 4,
    context: null,
    runtime_default: DEFAULT,
    startup: ["CLAUDE.md"],
    repo_overrides: [],
    repo: CODESENSE,
    defaulted: true,
    entries: [
      { path: "CLAUDE.md", source: "startup", files: 1, bytes: 10 },
      { path: ".claude/commands", source: "default", files: 4, bytes: 40 },
      { path: ".claude/skills", source: "default", files: 1209, bytes: 19_000_000 },
    ],
    roots: ROOTS,
    files: [
      file("CLAUDE.md"),
      file(".claude/skills/review/SKILL.md"),
      file(".claude/skills/review/ref.md"),
      file("docs/repo-only.md", REPO),
    ],
    truncated: false,
    totals: { files: 1214, bytes: 19_084_000 },
    warnings: [],
    ...over,
  };
}

describe("SW-XS-15 header words (AC-1)", () => {
  it("reads 'Context for Barb · in codesense' and '1,214 files · 18.2 MB'", () => {
    expect(contextHeaderLabel("Barb", "codesense")).toBe("Context for Barb · in codesense");
    expect(contextHeaderLabel("Barb")).toBe("Context for Barb");
    expect(contextSummary({ files: 1214, bytes: 19_084_000 })).toBe("1,214 files · 18.2 MB");
    expect(contextSummary({ files: 1, bytes: 512 })).toBe("1 file · 512 B");
  });

  it("names a team source repo as host/owner/repo, and matches older owner/repo saves", () => {
    expect(repoRequestLabel({ owner: "alderco", repo: "codesense" })).toBe(CODESENSE);
    expect(repoRequestLabel({ owner: "a", repo: "b", host: "gitlab.com" })).toBe("gitlab.com/a/b");
    expect(repoSlugMatches("alderco/codesense", { owner: "alderco", repo: "codesense" })).toBe(
      true,
    );
    expect(repoSlugMatches("gitlab.com/a/b", { owner: "a", repo: "b", host: "gitlab.com" })).toBe(
      true,
    );
    expect(repoSlugMatches("a/b", { owner: "a", repo: "b", host: "gitlab.com" })).toBe(false);
  });
});

describe("SW-XS-15 globs (the resolver's remove matcher)", () => {
  it("* and ? stay in one folder, ** spans folders, classes and alternatives work", () => {
    expect(globMatcher(".claude/skills/legacy-*")(".claude/skills/legacy-a")).toBe(true);
    expect(globMatcher(".claude/skills/legacy-*")(".claude/skills/legacy-a/SKILL.md")).toBe(false);
    expect(globMatcher("docs/**/draft.md")("docs/a/b/draft.md")).toBe(true);
    expect(globMatcher("docs/**/draft.md")("docs/draft.md")).toBe(true);
    expect(globMatcher("docs/v?")("docs/v1")).toBe(true);
    expect(globMatcher("docs/[!a]x")("docs/bx")).toBe(true);
    expect(globMatcher("docs/[!a]x")("docs/ax")).toBe(false);
    expect(globMatcher("{docs,notes}/x")("notes/x")).toBe(true);
    expect(globMatcher("Docs/X")("docs/x")).toBe(true);
  });

  it("an escaped literal path matches only itself", () => {
    const literal = "docs/[draft] *notes*";
    expect(escapeGlob(literal)).toBe("docs/[[]draft[]] [*]notes[*]");
    expect(globMatcher(escapeGlob(literal))(literal)).toBe(true);
    expect(globMatcher(escapeGlob(literal))("docs/d draft-notes-x")).toBe(false);
  });
});

describe("SW-XS-15 want-set membership (AC-1: the rest is muted)", () => {
  const m = buildContextMembership(barb());

  it("a file in the set is in, with its tier", () => {
    expect(m("CLAUDE.md")).toEqual({ state: "in", tier: "team" });
    expect(m("docs/repo-only.md")).toEqual({ state: "in", tier: "repo" });
  });

  it("a folder an entry covers is in; one with only some files in is partial", () => {
    expect(m(".claude/skills")).toEqual({ state: "in", tier: "team" });
    expect(m(".claude/skills/review")).toEqual({ state: "in", tier: "team" });
    expect(m(".claude")).toEqual({ state: "partial" });
    expect(m("docs")).toEqual({ state: "partial" });
    const mixed = buildContextMembership(
      barb({
        entries: [{ path: "docs", source: "context", files: 2, bytes: 2 }],
        files: [file("docs/a.md"), file("docs/b.md", REPO)],
      }),
    );
    expect(mixed("docs")).toEqual({ state: "in", mixed: true });
  });

  it("_bmad-output, and anything else outside the set, is out (muted)", () => {
    expect(m("_bmad-output")).toEqual({ state: "out" });
    expect(m("_bmad-output/04-sync-engine/SPEC.md")).toEqual({ state: "out" });
    expect(m(".claude/settings.local.json")).toEqual({ state: "out" });
    expect(m("")).toEqual({ state: "out" });
  });

  it("past the file cap, a covered path reads as in unless the repo override removes it", () => {
    const cut = buildContextMembership(
      barb({
        files: [],
        truncated: true,
        repo_overrides: [{ repo: CODESENSE, add: [], remove: [".claude/skills/legacy-*"] }],
      }),
    );
    expect(cut(".claude/skills/zeta/SKILL.md")).toEqual({ state: "in" });
    expect(cut(".claude/skills/legacy-a/SKILL.md")).toEqual({ state: "out" });
    expect(cut("_bmad-output/x.md")).toEqual({ state: "out" });
  });
});

describe("SW-XS-15 edit prediction (mirrors the server; AC-2)", () => {
  it("'Add to Barb's context for codesense only' is an edit; a team-wide add makes the default explicit", () => {
    expect(planAdd(barb(), "docs/architecture", CODESENSE)).toEqual({
      kind: "edit",
      optimistic: "in",
      makesExplicit: false,
    });
    expect(planAdd(barb(), "docs")).toEqual({
      kind: "edit",
      optimistic: "in",
      makesExplicit: true,
    });
    expect(planAdd(barb({ context: [{ path: "CLAUDE.md" }] }), "docs")).toEqual({
      kind: "edit",
      optimistic: "in",
      makesExplicit: false,
    });
  });

  it("covered adds and inner team-wide removes are refused, naming the entry", () => {
    expect(planAdd(barb(), ".claude/skills/review")).toEqual({
      kind: "blocked",
      reason: "Already included through .claude/skills.",
    });
    const inner = planRemove(barb(), ".claude/skills/review");
    expect(inner.kind === "blocked" && inner.reason).toMatch(/Included through \.claude\/skills/);
  });

  it("a repo-only add under a glob removal is refused; adding back an exact removal is an edit", () => {
    const view = barb({
      repo_overrides: [
        { repo: CODESENSE, add: [], remove: [".claude/skills/legacy-*", ".claude/skills/old"] },
      ],
    });
    const outer = planAdd(view, ".claude/skills/legacy-a/SKILL.md", CODESENSE);
    expect(outer.kind === "blocked" && outer.reason).toMatch(/through \.claude\/skills\/legacy-\*/);
    expect(planAdd(view, ".claude/skills/old", CODESENSE).kind).toBe("edit");
  });

  it("'Remove … for codesense only' is an edit; a startup file is never removed", () => {
    expect(planRemove(barb(), ".claude/skills/review", CODESENSE).kind).toBe("edit");
    const pinned = planRemove(barb(), "CLAUDE.md", CODESENSE);
    expect(pinned.kind === "blocked" && pinned.reason).toMatch(/Barb starts with CLAUDE\.md/);
  });

  it("a removal already in place is refused; a repo-only add can be removed", () => {
    const view = barb({
      repo_overrides: [
        { repo: CODESENSE, add: [{ path: "docs/architecture" }], remove: [".claude/skills/*"] },
      ],
    });
    expect(planRemove(view, ".claude/skills/review", CODESENSE).kind).toBe("blocked");
    expect(planRemove(view, "docs/architecture", CODESENSE).kind).toBe("edit");
  });
});

describe("SW-XS-15 r2", () => {
  it("the TS glob matcher agrees with the resolver on the shared fixture", () => {
    const cases = JSON.parse(
      readFileSync(
        resolve(__dirname, "../../../../src-tauri/src/agents/testdata/remove_glob_cases.json"),
        "utf8",
      ),
    ) as Array<{ glob: string; path: string; matches: boolean }>;
    expect(cases.length).toBeGreaterThanOrEqual(10);
    for (const c of cases) {
      expect([c.glob, c.path, globMatcher(c.glob)(c.path)]).toEqual([c.glob, c.path, c.matches]);
    }
  });

  it("adding back a path a broader glob matches is refused; only exact or inner removals drop", () => {
    const view = barb({
      repo_overrides: [
        {
          repo: CODESENSE,
          add: [],
          remove: [".claude/skills/legacy-*", escapeGlob(".claude/skills/[old]")],
        },
      ],
    });
    expect(planAdd(view, ".claude/skills/legacy-a", CODESENSE)).toEqual({
      kind: "blocked",
      reason:
        "Removed for this repo through .claude/skills/legacy-*; edit that pattern in the agent editor.",
    });
    expect(planAdd(view, ".claude/skills/[old]", CODESENSE).kind).toBe("edit");
  });

  it("a covered folder with a subfolder removed for the repo reads partial", () => {
    const m = buildContextMembership(
      barb({ repo_overrides: [{ repo: CODESENSE, add: [], remove: [".claude/skills/review"] }] }),
    );
    expect(m(".claude/skills")).toEqual({ state: "partial" });
  });
});

// SW-XS-16 r2 (N1): the nav's prediction judges by place, as `context_edit::plan` does.
describe("edit prediction by place", () => {
  const view = (context: AgentContextEntry[]): AgentContextView => ({ ...barb(), context });

  it("a nested remap is its own entry: removable, and not covered by its parent", () => {
    const v = view([{ path: ".claude/skills" }, { path: ".claude/skills/foo", dest: "foo" }]);
    expect(planRemove(v, ".claude/skills/foo").kind).toBe("edit");
    expect(planRemove(v, ".claude/skills/bar").kind).toBe("blocked");
    const remapped = view([{ path: "docs", dest: "kb" }]);
    expect(planAdd(remapped, "docs/arch").kind).toBe("edit");
    expect(placesLike({ path: "docs", dest: "kb" }, { path: "docs/arch", dest: "kb/arch" })).toBe(
      true,
    );
  });
});
