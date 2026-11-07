// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { scopePublishSet, stripContentPrefix, contentPrefix } from "./scope-publish-set";
import type { PublishFile, PublishSet } from "./types";

const file = (path: string): PublishFile => ({ path, status: "modified" });

const PREFIX = "_team/default/content";

function setOf(...paths: string[]): PublishSet {
  return { buckets: [], general: { files: paths.map(file), count: paths.length } };
}

describe("contentPrefix", () => {
  it("slices the org-root-relative prefix for a child content path", () => {
    expect(
      contentPrefix(
        "/orgs/acme/codesense-context",
        "/orgs/acme/codesense-context/_team/default/content",
      ),
    ).toBe("_team/default/content");
  });

  it("tolerates trailing slashes on either path", () => {
    expect(contentPrefix("/orgs/acme/ctx/", "/orgs/acme/ctx/_team/x/content/")).toBe(
      "_team/x/content",
    );
  });

  it("returns null when teamContentPath is empty (unhydrated)", () => {
    expect(contentPrefix("/orgs/acme/ctx", "")).toBeNull();
    expect(contentPrefix("/orgs/acme/ctx", undefined)).toBeNull();
  });

  it("returns null when orgRoot is null", () => {
    expect(contentPrefix(null, "/orgs/acme/ctx/_team/x/content")).toBeNull();
  });

  it("returns null when teamContentPath is not a child of orgRoot", () => {
    expect(contentPrefix("/orgs/acme/ctx", "/orgs/other/ctx/_team/x/content")).toBeNull();
    expect(contentPrefix("/orgs/acme/ctx", "/orgs/acme/ctx")).toBeNull();
  });
});

describe("scopePublishSet", () => {
  it("keeps only files under the prefix and recomputes the general count", () => {
    const set = setOf(
      `${PREFIX}/_bmad-output/a.md`,
      `${PREFIX}/b.md`,
      "_team/other/content/c.md",
      ".meta/root.json",
    );
    const scoped = scopePublishSet(set, PREFIX);
    expect(scoped.general.files.map((f) => f.path)).toEqual([
      `${PREFIX}/_bmad-output/a.md`,
      `${PREFIX}/b.md`,
    ]);
    expect(scoped.general.count).toBe(2);
  });

  it("filters every bucket too and prunes emptied buckets", () => {
    const set: PublishSet = {
      buckets: [
        {
          workspaceRef: { id: "w1", label: "story-1", kind: "story" },
          count: 2,
          files: [file(`${PREFIX}/x.md`), file("_team/other/content/y.md")],
        },
        {
          workspaceRef: { id: "w2", label: "story-2", kind: "story" },
          count: 1,
          files: [file("_team/other/content/z.md")],
        },
      ],
      general: { files: [], count: 0 },
    };
    const scoped = scopePublishSet(set, PREFIX);
    expect(scoped.buckets).toHaveLength(1);
    expect(scoped.buckets[0].count).toBe(1);
    expect(scoped.buckets[0].files.map((f) => f.path)).toEqual([`${PREFIX}/x.md`]);
  });

  it("returns the set UNCHANGED for a null prefix (hydration passthrough)", () => {
    const set = setOf(`${PREFIX}/a.md`, "_team/other/content/b.md");
    expect(scopePublishSet(set, null)).toBe(set);
  });

  it("yields an empty set when nothing matches the prefix", () => {
    const set = setOf("_team/other/content/a.md", ".meta/root.json");
    const scoped = scopePublishSet(set, PREFIX);
    expect(scoped.general.files).toHaveLength(0);
    expect(scoped.general.count).toBe(0);
    expect(scoped.buckets).toHaveLength(0);
  });
});

describe("stripContentPrefix", () => {
  it("strips the prefix for a child path", () => {
    expect(stripContentPrefix(`${PREFIX}/_bmad-output/x/foo.md`, PREFIX)).toBe(
      "_bmad-output/x/foo.md",
    );
  });

  it("leaves a non-prefixed path unchanged", () => {
    expect(stripContentPrefix("_team/other/content/foo.md", PREFIX)).toBe(
      "_team/other/content/foo.md",
    );
    expect(stripContentPrefix(".meta/root.json", PREFIX)).toBe(".meta/root.json");
  });

  it("leaves the path unchanged for a null prefix", () => {
    expect(stripContentPrefix(`${PREFIX}/foo.md`, null)).toBe(`${PREFIX}/foo.md`);
  });
});

it("scopes private content to one org without including notes, config, all-orgs or sibling orgs", () => {
  const set = setOf(
    "orgs/acme/a/content/idea.md",
    "orgs/acme/a/notes/n.md",
    "orgs/acme/a/config/teams.yml",
    "orgs/acme/b/content/secret.md",
    "orgs/acme/ab/content/secret.md",
    "all-orgs/prompts/x.md",
  );
  expect(scopePublishSet(set, "orgs/acme/a/content").general.files.map((f) => f.path)).toEqual([
    "orgs/acme/a/content/idea.md",
  ]);
});
