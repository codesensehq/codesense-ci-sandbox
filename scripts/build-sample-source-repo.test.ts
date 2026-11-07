// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

// @ts-expect-error -- plain Node ESM build script without type declarations.
import {
  buildCommitTrees,
  buildInputs,
  buildRepository,
  tarGz,
} from "./build-sample-source-repo.mjs";

/** Minimal commit list fixture used by the arity / order tests. */
function fakeCommits() {
  return [
    { name: "01-first.json", data: { message: "first", files: { "a.txt": "a" } } },
    {
      name: "02-second.json",
      data: { message: "second", files: { "b.txt": "b", "a.txt": "a2" } },
    },
    { name: "03-third.json", data: { message: "third", files: { "c.txt": "c" } } },
  ];
}

describe("sample-source-repo build (SW-ODC-11)", () => {
  it("accumulates files across commits: later commits override earlier ones", () => {
    const trees = buildCommitTrees(fakeCommits()) as Map<string, Buffer>[];
    expect(trees).toHaveLength(3);
    expect([...trees[0].keys()].sort()).toEqual(["a.txt"]);
    expect([...trees[1].keys()].sort()).toEqual(["a.txt", "b.txt"]);
    expect([...trees[2].keys()].sort()).toEqual(["a.txt", "b.txt", "c.txt"]);
    // The override lands: commit 2's a.txt is "a2".
    expect(trees[1].get("a.txt")!.toString("utf8")).toBe("a2");
  });

  it("buildRepository emits one commit per manifest and a chain of parents", () => {
    const commits = fakeCommits();
    const trees = buildCommitTrees(commits) as Map<string, Buffer>[];
    const { entries, commit } = buildRepository(commits, trees);
    expect(commit).toHaveLength(40);
    // Must have exactly one HEAD, config, two branch refs, plus object files.
    const names = entries.map(([n]: [string, Buffer]) => n);
    expect(names).toContain(".git/HEAD");
    expect(names).toContain(".git/config");
    expect(names).toContain(".git/refs/heads/main");
    expect(names).toContain(".git/refs/remotes/origin/main");
    // Three commits + three trees + distinct blobs = 9 unique objects at minimum.
    const objects = names.filter((n: string) => n.startsWith(".git/objects/")).length;
    expect(objects).toBeGreaterThanOrEqual(9);
  });

  it("tarGz is deterministic byte-for-byte for the same inputs", () => {
    const commits = fakeCommits();
    const trees = buildCommitTrees(commits) as Map<string, Buffer>[];
    const { entries } = buildRepository(commits, trees);
    const a = tarGz(entries, 1_790_812_800) as Buffer;
    const b = tarGz(entries, 1_790_812_800) as Buffer;
    expect(Buffer.compare(a, b)).toBe(0);
  });

  it("declares exactly its inputs for cargo:rerun-if-changed", () => {
    const inputs = (buildInputs() as string[]).map((p: string) =>
      p.replace(`${process.cwd()}/`, ""),
    );
    expect(inputs).toContain("scripts/build-sample-source-repo.mjs");
    expect(inputs).toContain("codesense-docs/sample-source-repo");
    // The two tarballs are built independently; sample-source-repo must NOT
    // pull in the whole codesense-docs/ tree as a rerun input.
    expect(inputs).not.toContain("codesense-docs");
    expect(inputs.some((p: string) => p.endsWith("/handbook") || p.endsWith("/docs"))).toBe(false);
  });

  it("rejects a commit manifest with a non-string file body", () => {
    const bad = [
      {
        name: "01.json",
        data: { message: "x", files: { "a.txt": 42 as unknown as string } },
      },
    ];
    expect(() => buildCommitTrees(bad)).toThrow(/content must be a string/);
  });

  it("rejects a commit manifest with no files object", () => {
    const bad = [{ name: "01.json", data: { message: "x" } }];
    expect(() => buildCommitTrees(bad)).toThrow(/'files' object/);
  });
});
