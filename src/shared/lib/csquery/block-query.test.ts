// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { blockPage, blockQuery, CONNECTIVITY_LIMIT } from "./block-query";
import { parseCsQuery } from "./parse-csquery";
import { isCsQueryError, type CsQuerySpec } from "./types";

function spec(src: string): CsQuerySpec {
  const r = parseCsQuery(src);
  if (isCsQueryError(r)) throw new Error(r.error);
  return r;
}

const BASE = "/ctx/acme/default/content/_bmad-output/13-x/README.md";

describe("blockQuery — a block's UI defaults on top of its parsed query", () => {
  it("scopes to the block's own org unless the block names orgs", () => {
    expect(blockQuery(spec("status: todo"), { basePath: BASE }).where?.sameOrgAs).toBe(BASE);
    const all = blockQuery(spec("org: all"), { basePath: BASE });
    expect(all.where?.sameOrgAs).toBeUndefined();
    expect(all.where?.orgs).toBe("all");
    // No containing doc → no implicit scope at all (footprint-wide).
    expect(blockQuery(spec("status: todo"), {}).where?.sameOrgAs).toBeUndefined();
  });

  it("resolves relative relationship refs against the block's folder; ids pass through", () => {
    const q = blockQuery(spec("related: ./notes.md\nbacklink: SW-OC\ndepends: ../y/S-1.md"), {
      basePath: BASE,
    });
    expect(q.relationships).toEqual({
      hasSupportingDoc: "/ctx/acme/default/content/_bmad-output/13-x/notes.md",
      backlinkedBy: "SW-OC",
      dependsOn: "/ctx/acme/default/content/_bmad-output/y/S-1.md",
    });
  });

  it("bounds by a resolved scope root; caps by the view's limit; boards group by status", () => {
    const q = blockQuery(spec("scope: 13-x\nlimit: 25\nview: board"), {
      basePath: BASE,
      scopeRoot: "/ctx/acme/default/content/_bmad-output/13-x",
    });
    expect(q.where?.pathPrefix).toBe("/ctx/acme/default/content/_bmad-output/13-x");
    expect(q.limit).toBe(25);
    expect(q.groupBy).toBe("status");
    const conn = blockQuery(spec("view: connectivity\nscope: /abs/epic"), {});
    expect(conn.limit).toBe(CONNECTIVITY_LIMIT);
    expect(conn.where?.pathPrefix).toBe("/abs/epic");
  });

  it("status sorts take the team's taxonomy order; other keys are untouched", () => {
    const order = ["shipped", "building"];
    expect(blockQuery(spec(""), { statusOrder: order }).sort).toEqual([
      { key: "status", dir: "asc", order },
      { key: "story_id", missing: "first" },
    ]);
    expect(blockQuery(spec("sort: status desc"), { statusOrder: order }).sort).toEqual([
      { key: "status", dir: "desc", order },
    ]);
    expect(blockQuery(spec("sort: priority"), { statusOrder: order }).sort).toEqual([
      { key: "priority", dir: "asc" },
    ]);
  });

  it("runs for the containing doc as its page; no doc means no private items", () => {
    expect(blockPage(BASE)).toBe(BASE);
    expect(blockPage(undefined)).toBe("");
  });

  it("never mutates the parsed spec", () => {
    const s = spec("related: ./a.md");
    const before = JSON.stringify(s);
    blockQuery(s, { basePath: BASE, scopeRoot: "/x" });
    expect(JSON.stringify(s)).toBe(before);
  });
});
