// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { parseCsQuery } from "./parse-csquery";
import { DEFAULT_CSQUERY_LIMIT, isCsQueryError, type CsQuerySpec } from "./types";

/** Parse and assert it's the spec arm (not an error) — returns the spec. */
function spec(src: string): CsQuerySpec {
  const r = parseCsQuery(src);
  if (isCsQueryError(r)) throw new Error(`expected spec, got error: ${r.error}`);
  return r;
}

const DEFAULT_WHERE = { kinds: ["story", "bug", "spike"] };
const DEFAULT_SORT = ["status", { key: "story_id", missing: "first" }];

describe("parseCsQuery — grammar → ContextQuery", () => {
  it("is pure: same input → deep-equal output, no throw", () => {
    const src = "tag: type=epic\nstatus: done";
    expect(parseCsQuery(src)).toEqual(parseCsQuery(src));
  });

  it("typed tag `key=value` → an eq filter on that key", () => {
    expect(spec("tag: type=epic").query.filters).toEqual([
      { key: "type", op: "eq", value: "epic" },
    ]);
  });

  it("bare `tag: x` → the conventional `tags` field", () => {
    expect(spec("tag: epic").query.filters).toEqual([{ key: "tags", op: "eq", value: "epic" }]);
  });

  it("repeated same-key tags stay separate clauses (the engine ORs a key)", () => {
    expect(spec("tag: team=a\ntag: team=b").query.filters).toEqual([
      { key: "team", op: "eq", value: "a" },
      { key: "team", op: "eq", value: "b" },
    ]);
  });

  it("comma `status:` → one `in` clause", () => {
    expect(spec("status: todo, in-progress ,done").query.filters).toEqual([
      { key: "status", op: "in", value: ["todo", "in-progress", "done"] },
    ]);
  });

  it("related / backlink / epic / depends → relationship filter", () => {
    const s = spec(
      "related: SW-DX-2-query-layer.md\nbacklink: ../epic/README.md\nepic: SW-OC\ndepends: SW-OC-25",
    );
    expect(s.query.relationships).toEqual({
      hasSupportingDoc: "SW-DX-2-query-layer.md",
      backlinkedBy: "../epic/README.md",
      parentEpic: "SW-OC",
      dependsOn: "SW-OC-25",
    });
  });

  it("kind / org / team / layer / text map onto the query", () => {
    const s = spec(
      "kind: story, epic\norg: acme, globex\nteam: default\nlayer: private\ntext: login",
    );
    expect(s.query.where).toEqual({
      kinds: ["story", "epic"],
      orgs: ["acme", "globex"],
      teams: ["default"],
      layers: ["private"],
    });
    expect(s.query.text).toBe("login");
    expect(s.orgExplicit).toBe(true);
    expect(spec("org: all").query.where?.orgs).toBe("all");
  });

  it("defaults: pipeline kinds, status order, the block's own org", () => {
    const s = spec("");
    expect(s.query).toEqual({ where: DEFAULT_WHERE, sort: DEFAULT_SORT });
    expect(s.orgExplicit).toBe(false);
  });

  it("unknown kind / layer values warn and are dropped", () => {
    const s = spec("kind: story, gizmo\nlayer: moon");
    expect(s.query.where?.kinds).toEqual(["story"]);
    expect(s.query.where?.layers).toBeUndefined();
    expect(s.warnings.some((w) => w.includes("gizmo"))).toBe(true);
    expect(s.warnings.some((w) => w.includes("layer"))).toBe(true);
  });

  it("ignores blank lines and `#` comments", () => {
    const s = spec("\n# a comment\n\nstatus: done\n   # indented comment\n");
    expect(s.query.filters).toEqual([{ key: "status", op: "in", value: ["done"] }]);
    expect(s.warnings).toEqual([]);
  });

  it("keys are case-insensitive and whitespace-tolerant", () => {
    const s = spec("  STATUS :  done  \n  Tag:  type=Epic ");
    expect(s.query.filters).toEqual([
      { key: "type", op: "eq", value: "Epic" },
      { key: "status", op: "in", value: ["done"] },
    ]);
  });

  it("view hints: table/list/board", () => {
    expect(spec("view: list").view).toBe("list");
    expect(spec("view: BOARD").view).toBe("board");
    expect(spec("").view).toBe("table"); // default
  });

  it("unknown view → default table WITH a warning (not an error)", () => {
    const s = spec("view: gantt");
    expect(s.view).toBe("table");
    expect(s.warnings.some((w) => w.includes("gantt"))).toBe(true);
  });

  it("columns default to name,status and override via `columns:`", () => {
    expect(spec("").columns).toEqual(["name", "status"]);
    expect(spec("columns: name, status, team").columns).toEqual(["name", "status", "team"]);
  });

  it("limit defaults and parses; invalid or 0 → default + warning", () => {
    expect(spec("").limit).toBe(DEFAULT_CSQUERY_LIMIT);
    expect(spec("limit: 5").limit).toBe(5);
    const bad = spec("limit: lots");
    expect(bad.limit).toBe(DEFAULT_CSQUERY_LIMIT);
    expect(bad.warnings.some((w) => w.includes("limit"))).toBe(true);
    const zero = spec("limit: 0");
    expect(zero.limit).toBe(DEFAULT_CSQUERY_LIMIT);
    expect(zero.warnings.some((w) => w.includes("limit"))).toBe(true);
  });

  it("a repeated single-value key last-wins WITH a warning (incl. columns)", () => {
    const s = spec("view: list\nview: board");
    expect(s.view).toBe("board");
    expect(s.warnings.some((w) => w.includes("more than once"))).toBe(true);
    const c = spec("columns: name\ncolumns: name, team");
    expect(c.columns).toEqual(["name", "team"]);
    expect(c.warnings.some((w) => w.includes("more than once"))).toBe(true);
    // Multi-value keys (tag/status) do NOT warn on repeat.
    expect(spec("tag: a=1\ntag: b=2").warnings).toEqual([]);
  });

  it("an ignored empty directive does not provoke a spurious repeat-warning", () => {
    const s = spec("related:\nrelated: SW-DX-2");
    expect(s.query.relationships?.hasSupportingDoc).toBe("SW-DX-2");
    expect(s.warnings.some((w) => w.includes("more than once"))).toBe(false);
  });

  it("sort takes any field; aliases map; direction carries into the query", () => {
    expect(spec("sort: name").sort).toEqual({ field: "fileName", dir: "asc" });
    expect(spec("sort: status desc").query.sort).toEqual([{ key: "status", dir: "desc" }]);
    expect(spec("sort: id").query.sort).toEqual([{ key: "story_id", dir: "asc" }]);
    expect(spec("sort: priority").query.sort).toEqual([{ key: "priority", dir: "asc" }]);
  });

  it("unknown key → warning, not error", () => {
    const s = spec("frobnicate: yes\nstatus: done");
    expect(s.query.filters).toEqual([{ key: "status", op: "in", value: ["done"] }]);
    expect(s.warnings.some((w) => w.includes("frobnicate"))).toBe(true);
  });

  it("a line with no colon → warning + skip", () => {
    const s = spec("this is not a directive\nstatus: done");
    expect(s.query.filters).toHaveLength(1);
    expect(s.warnings.some((w) => w.includes("not a directive"))).toBe(true);
  });

  it("entirely-unrecognized body → default query + warnings, still a spec", () => {
    const s = spec("garbage\nmore garbage\n!!!");
    expect(s.query).toEqual({ where: DEFAULT_WHERE, sort: DEFAULT_SORT });
    expect(s.warnings.length).toBeGreaterThan(0);
  });
});

describe("parseCsQuery — read-only invariant (AC-11)", () => {
  for (const verb of ["setStatus", "fire", "open", "draft", "route", "run"]) {
    it(`drops action verb "${verb}" with a warning and still renders as a read view`, () => {
      const s = spec(`${verb}: SW-DX-2\nstatus: done`);
      expect(JSON.stringify(s.query)).not.toContain(verb);
      expect(s.query.filters).toEqual([{ key: "status", op: "in", value: ["done"] }]);
      expect(s.warnings.some((w) => w.toLowerCase().includes("action"))).toBe(true);
    });
  }
});

describe("parseCsQuery — connectivity mode + scope (SW-DX-8)", () => {
  it("recognizes `view: connectivity`", () => {
    expect(spec("view: connectivity").view).toBe("connectivity");
  });

  it("`scope:` is kept raw for the island to resolve (sub-epic name or path)", () => {
    expect(spec("view: connectivity\nscope: open-dashboard-runtime").scope).toBe(
      "open-dashboard-runtime",
    );
    expect(spec("scope: /ctx/epic-a").scope).toBe("/ctx/epic-a");
  });

  it("empty `scope:` is ignored with a warning", () => {
    const s = spec("view: connectivity\nscope:");
    expect(s.scope).toBeNull();
    expect(s.warnings.some((w) => w.toLowerCase().includes("scope"))).toBe(true);
  });

  it("a duplicate `scope:` warns (last wins)", () => {
    const s = spec("scope: a\nscope: b");
    expect(s.scope).toBe("b");
    expect(s.warnings.some((w) => w.toLowerCase().includes("more than once"))).toBe(true);
  });
});
