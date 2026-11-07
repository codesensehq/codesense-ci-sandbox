// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import golden from "../doc-query/__fixtures__/golden-query-result.json";
import type { DocQueryResult } from "../doc-query/types";
import { buildConnectivityView } from "./build-connectivity-view";

const here = dirname(fileURLToPath(import.meta.url));

describe("connectivity: no resolver fork (AC-3)", () => {
  it("no connectivity module resolves paths itself (imports no resolveRelativePath)", () => {
    const files = readdirSync(here).filter((f) => f.endsWith(".ts") && !f.endsWith(".test.ts"));
    expect(files.length).toBeGreaterThan(0);
    for (const f of files) {
      const src = readFileSync(join(here, f), "utf8");
      expect(src, `${f} must not do path resolution`).not.toMatch(
        /resolve-relative-path|resolveRelativePath/,
      );
    }
  });

  it("forward edges equal the engine's supportingDocs for the same center (parity)", () => {
    // The builder consumes the SAME resolved edges the Rust engine emits (the
    // committed golden is its output over sample-tree.json) — one resolver, no fork.
    const docs = (golden as DocQueryResult).docs;
    const view = buildConnectivityView(docs, "/ctx/epic-a");
    const centerQuery = docs.find((d) => d.fileName === "a1-foundation.md")!;
    const centerRow = view.rows.find((r) => r.story.fileName === "a1-foundation.md")!;
    expect(centerRow.forward.map((l) => l.filePath).sort()).toEqual(
      centerQuery.relationships.supportingDocs.map((r) => r.path).sort(),
    );
    // …and the resolved/unresolved flags carry through unchanged.
    const arch = centerRow.forward.find((l) => l.fileName === "arch-notes.excalidraw");
    const dead = centerRow.forward.find((l) => l.fileName === "diagram.bin");
    expect(arch?.resolved).toBe(true);
    expect(dead?.resolved).toBe(false);
  });
});
