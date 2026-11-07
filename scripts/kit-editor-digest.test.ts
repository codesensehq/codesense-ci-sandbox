// SPDX-License-Identifier: MIT
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

// @ts-expect-error -- plain Node ESM, no .d.ts.
import { bundledPackages, collectGraph, editorDigest } from "./kit-editor-digest.mjs";

const lockfile = (
  versions: {
    core?: string;
    pm?: string;
    ajv?: string;
    alias?: string;
    scopedImporter?: string;
    scopedSnapshot?: string;
    vite?: string;
    esbuild?: string;
  } = {},
) => `lockfileVersion: '9.0'

importers:

  .:
    dependencies:
      '@tiptap/core':
        specifier: ^3.0.0
        version: ${versions.core ?? "3.22.3"}(@tiptap/pm@3.22.3)
      ajv:
        specifier: ^8.0.0
        version: ${versions.ajv ?? "8.18.0"}
      editor-dep:
        specifier: npm:real-editor-dep@^1.0.0
        version: real-editor-dep@1.0.0(peer-dep@2.0.0)
      scoped-dep:
        specifier: npm:@scope/real@^1.0.0
        version: '@scope/real@1.0.0(peer-dep@2.0.0)'
      vite:
        specifier: ^7.0.0
        version: ${versions.vite ?? "7.3.1"}

packages:

  '@tiptap/core@3.22.3':
    resolution: {integrity: sha512-a}

snapshots:

  '@tiptap/core@${versions.core ?? "3.22.3"}(@tiptap/pm@3.22.3)':
    dependencies:
      '@tiptap/pm': ${versions.pm ?? "3.22.3"}

  '@tiptap/pm@${versions.pm ?? "3.22.3"}':
    dependencies:
      prosemirror-model: 1.0.0

  prosemirror-model@1.0.0: {}

  ajv@${versions.ajv ?? "8.18.0"}:
    dependencies:
      fast-deep-equal: 3.1.3

  fast-deep-equal@3.1.3: {}

  real-editor-dep@1.0.0(peer-dep@2.0.0):
    dependencies:
      inner: ${versions.alias ?? "1.0.0"}

  inner@${versions.alias ?? "1.0.0"}: {}

  '@scope/real@1.0.0(peer-dep@2.0.0)':
    dependencies:
      scoped-inner: '@scope/inner@1.0.0'
      leaf-one: ${versions.scopedImporter ?? "1.0.0"}

  leaf-one@${versions.scopedImporter ?? "1.0.0"}: {}

  '@scope/inner@1.0.0':
    dependencies:
      leaf-two: ${versions.scopedSnapshot ?? "1.0.0"}

  leaf-two@${versions.scopedSnapshot ?? "1.0.0"}: {}

  vite@${versions.vite ?? "7.3.1"}:
    dependencies:
      esbuild: ${versions.esbuild ?? "0.27.4"}
      aliased-esbuild: esbuild-real@1.0.0

  esbuild@${versions.esbuild ?? "0.27.4"}: {}

  esbuild-real@1.0.0: {}
`;

describe("kit editor stamp digest", () => {
  let root: string;
  const digest = () => editorDigest(root, resolve(root, "src/entry.ts")).digest as string;
  const writeLock = (versions?: Parameters<typeof lockfile>[0]) =>
    writeFileSync(resolve(root, "pnpm-lock.yaml"), lockfile(versions));

  beforeEach(() => {
    root = mkdtempSync(resolve(tmpdir(), "kit-digest-"));
    mkdirSync(resolve(root, "scripts"));
    mkdirSync(resolve(root, "src/styles"), { recursive: true });
    for (const file of [
      "scripts/build-kit-editor.mjs",
      "scripts/kit-editor-digest.mjs",
      "src/styles/scratch-note.css",
      "src/styles/status-mark.css",
    ])
      writeFileSync(resolve(root, file), "x");
    writeFileSync(
      resolve(root, "scripts/build-kit-editor.mjs"),
      `import { build } from "vite";\nimport "node:fs";\n`,
    );
    writeFileSync(
      resolve(root, "src/entry.ts"),
      `import { Editor } from "@tiptap/core";\nimport "editor-dep";\nimport "scoped-dep";\nimport "node:fs";\nimport { helper } from "./helper";\n`,
    );
    writeFileSync(resolve(root, "src/helper.ts"), "export const helper = 1;\n");
    writeLock();
  });
  afterEach(() => rmSync(root, { recursive: true, force: true }));

  it("ignores a lockfile package the editor does not bundle", () => {
    const before = digest();
    writeLock({ ajv: "8.99.0" });
    expect(digest()).toBe(before);
  });

  it("changes when a bundled package version changes", () => {
    const before = digest();
    writeLock({ core: "3.23.0" });
    expect(digest()).not.toBe(before);
  });

  it("changes when a transitive dependency of a bundled package changes", () => {
    const before = digest();
    writeLock({ pm: "3.23.0" });
    expect(digest()).not.toBe(before);
  });

  it("changes when a transitive dependency of an npm-aliased bundled package changes", () => {
    const before = digest();
    writeLock({ alias: "1.1.0" });
    expect(digest()).not.toBe(before);
  });

  it("changes when a dependency of a quoted scoped importer alias changes", () => {
    const before = digest();
    writeLock({ scopedImporter: "1.1.0" });
    expect(digest()).not.toBe(before);
  });

  it("changes when a dependency of a quoted scoped snapshot alias changes", () => {
    const before = digest();
    writeLock({ scopedSnapshot: "1.1.0" });
    expect(digest()).not.toBe(before);
  });

  it("changes when the bundler version changes", () => {
    const before = digest();
    writeLock({ vite: "7.3.2" });
    expect(digest()).not.toBe(before);
  });

  it("changes when a transitive dependency of the bundler changes", () => {
    const before = digest();
    writeLock({ esbuild: "0.27.5" });
    expect(digest()).not.toBe(before);
  });

  it("changes when a local input changes", () => {
    const before = digest();
    writeFileSync(resolve(root, "src/helper.ts"), "export const helper = 2;\n");
    expect(digest()).not.toBe(before);
  });

  it("collects bare imports but not node: builtins", () => {
    expect([...collectGraph(root, resolve(root, "src/entry.ts")).packages]).toEqual([
      "@tiptap/core",
      "editor-dep",
      "scoped-dep",
    ]);
  });

  it("does not treat type-only imports as inputs", () => {
    writeFileSync(resolve(root, "src/types-only.ts"), "export type T = 1;\n");
    writeFileSync(
      resolve(root, "src/entry.ts"),
      `import type { T } from "./types-only";\nimport type {\n  A,\n} from "type-only-package";\nexport type { T } from "./types-only";\nimport { helper } from "./helper";\n`,
    );
    const graph = collectGraph(root, resolve(root, "src/entry.ts"));
    expect([...graph.files].map((f: string) => f.replace(`${root}/`, ""))).toEqual([
      "src/entry.ts",
      "src/helper.ts",
    ]);
    expect([...graph.packages]).toEqual([]);
  });

  const graphFiles = (entrySource: string) => {
    writeFileSync(resolve(root, "src/entry.ts"), entrySource);
    return [...collectGraph(root, resolve(root, "src/entry.ts")).files].map((f: string) =>
      f.replace(`${root}/`, ""),
    );
  };

  it("keeps a runtime import that follows a semicolonless type alias", () => {
    writeFileSync(resolve(root, "src/dep.ts"), "export const Y = 1;\n");
    const source = `export type T = string\nimport { Y } from "./dep";\nexport const value = Y;\n`;
    expect(graphFiles(source)).toContain("src/dep.ts");
    const before = digest();
    writeFileSync(resolve(root, "src/dep.ts"), "export const Y = 2;\n");
    expect(digest()).not.toBe(before);
  });

  it("keeps a runtime default import whose binding is named type", () => {
    writeFileSync(resolve(root, "src/dep.ts"), "export default () => 1;\n");
    const source = `import type from "./dep";\nexport const value = type();\n`;
    expect(graphFiles(source)).toContain("src/dep.ts");
    const before = digest();
    writeFileSync(resolve(root, "src/dep.ts"), "export default () => 2;\n");
    expect(digest()).not.toBe(before);
  });

  it("keeps a mixed import with a runtime specifier and skips an all-type module", () => {
    writeFileSync(resolve(root, "src/dep.ts"), "export const domNodeViews = 1;\n");
    writeFileSync(resolve(root, "src/types-only.ts"), "export type DomViewOptions = 1;\n");
    const source = `import { domNodeViews, type DomViewOptions } from "./dep";\nimport type { DomViewOptions as D } from "./types-only";\nexport const value = domNodeViews;\n`;
    expect(graphFiles(source)).toEqual(["src/entry.ts", "src/dep.ts"]);
    const before = digest();
    writeFileSync(resolve(root, "src/dep.ts"), "export const domNodeViews = 2;\n");
    expect(digest()).not.toBe(before);
  });

  it("skips type-only import-equals but keeps side-effect imports and runtime re-exports", () => {
    writeFileSync(resolve(root, "src/types-only.ts"), "export type T = 1;\n");
    writeFileSync(resolve(root, "src/effect.ts"), "export {};\n");
    writeFileSync(resolve(root, "src/reexport.ts"), "export const R = 1;\n");
    const source = `import type X = require("./types-only");\nimport "./effect";\nexport { R } from "./reexport";\nexport * from "./reexport";\n`;
    expect(graphFiles(source)).toEqual(["src/entry.ts", "src/effect.ts", "src/reexport.ts"]);
  });
});

describe("real lockfile", () => {
  it("bundles TipTap and its transitive dependencies but not the docs-script ajv", () => {
    const entry = resolve(process.cwd(), "src/features/notes/editor/kit-entry.ts");
    const { packages } = collectGraph(process.cwd(), entry);
    for (const name of collectGraph(
      process.cwd(),
      resolve(process.cwd(), "scripts/build-kit-editor.mjs"),
    ).packages)
      packages.add(name);
    const lock = readFileSync(resolve(process.cwd(), "pnpm-lock.yaml"), "utf8");
    const resolved: string[] = bundledPackages(lock, packages);
    expect(resolved.some((name) => name.startsWith("@tiptap/core@"))).toBe(true);
    expect(resolved.some((name) => name.startsWith("prosemirror-model@"))).toBe(true);
    expect(resolved.some((name) => name.startsWith("vite@"))).toBe(true);
    expect(resolved.some((name) => name.startsWith("esbuild@"))).toBe(true);
    expect(resolved.some((name) => name.startsWith("ajv@"))).toBe(false);
  });
});
