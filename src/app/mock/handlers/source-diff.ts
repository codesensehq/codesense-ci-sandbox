// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { FileRedline, PublishFile } from "@/features/git-sync/lib/types";

/**
 * The source-diff tool's backend in `dev:mock`: one realistic story worktree
 * (nested source, a rename, a delete, markdown, a binary asset, a large file)
 * served identically through the LOCAL and REMOTE command pairs, so the tool can
 * be exercised end to end in a browser (`?harness=source-diff`, add `&remote=1`
 * for the machine path, `&diff=empty|error|slow|stale|large` for the edge states —
 * `stale` is an out-of-date terminal server on the machine path, `large` ~300 files
 * with a 5k-line rewrite).
 *
 * `base_ref: "HEAD"` narrows the set to the "uncommitted" subset, mirroring how
 * the real backend diffs against the branch tip instead of the fork point.
 */

interface FixtureFile {
  file: PublishFile;
  before: string | null;
  after: string | null;
  binary?: boolean;
  /** Part of the uncommitted (vs HEAD) subset. */
  uncommitted?: boolean;
}

const lines = (n: number, f: (i: number) => string) =>
  Array.from({ length: n }, (_, i) => f(i)).join("\n") + "\n";

const OVERLAY_BEFORE = `import { useState } from "react";

import { cn } from "@/lib/utils";
import type { PublishFile } from "@/features/git-sync/lib/types";

/** A one-line row in the changed-file rail. */
export function FileRow({ file, selected, onOpen }: FileRowProps) {
  return (
    <button
      type="button"
      onClick={onOpen}
      className={cn("flex items-center gap-2 px-2 py-1", selected && "bg-accent")}
    >
      <span className="truncate">{file.path}</span>
    </button>
  );
}

${lines(40, (i) => `// unchanged context line ${i + 1} keeps the hunks apart`)}
export function countChanges(files: PublishFile[]): number {
  let total = 0;
  for (const f of files) total += 1;
  return total;
}

${lines(30, (i) => `const SPACER_${i} = ${i};`)}
export function summarize(files: PublishFile[]) {
  return files.map((f) => f.path).join(", ");
}
`;

const OVERLAY_AFTER = `import { useMemo, useState } from "react";

import { cn } from "@/lib/utils";
import type { PublishFile } from "@/features/git-sync/lib/types";
import { StatusChip } from "./file-status-chip";

/** A one-line row in the changed-file rail: status, name, and its +/− weight. */
export function FileRow({ file, selected, onOpen }: FileRowProps) {
  const name = useMemo(() => file.path.split("/").pop() ?? file.path, [file.path]);
  return (
    <button
      type="button"
      onClick={onOpen}
      aria-selected={selected}
      className={cn("flex items-center gap-2 px-2 py-1", selected && "bg-accent")}
    >
      <StatusChip status={file.status} />
      <span className="truncate">{name}</span>
    </button>
  );
}

${lines(40, (i) => `// unchanged context line ${i + 1} keeps the hunks apart`)}
export function countChanges(files: PublishFile[]): { added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const f of files) {
    added += f.added ?? 0;
    removed += f.removed ?? 0;
  }
  return { added, removed };
}

${lines(30, (i) => `const SPACER_${i} = ${i};`)}
export function summarize(files: PublishFile[]) {
  return files.map((f) => \`\${f.status[0].toUpperCase()} \${f.path}\`).join("\\n");
}
`;

const RUST_BEFORE = `use std::path::Path;

/// Resolve the worktree's baseline tree.
pub fn baseline(repo: &Path) -> Result<String, String> {
    let head = rev_parse(repo, "HEAD")?;
    Ok(head)
}
`;

const RUST_AFTER = `use std::path::Path;

/// Resolve the worktree's baseline: its fork point when one resolves, else HEAD.
pub fn baseline(repo: &Path, base_ref: Option<&str>) -> Result<String, String> {
    let head = rev_parse(repo, "HEAD")?;
    for spec in base_ref.into_iter().chain(["origin/main", "main"]) {
        if let Ok(base) = merge_base(repo, &head, spec) {
            return Ok(base);
        }
    }
    Ok(head)
}
`;

const DOC_BEFORE = `# Source diff

Review what an agent changed in its worktree.

- Pick a file on the left.
- Read the diff on the right.
`;

const DOC_AFTER = `# Source diff

Review what an agent changed in its worktree — locally or on a machine.

- Pick a file on the left, or press **j** / **k**.
- Step through changes with **n** / **p**.
- Switch between the whole branch and uncommitted work.
`;

const BIG_AFTER = lines(2400, (i) => `export const ROW_${i} = { id: ${i}, label: "row ${i}" };`);

const FIXTURE: FixtureFile[] = [
  {
    file: {
      path: "src/features/viewer/components/file-row.tsx",
      status: "modified",
      added: 14,
      removed: 4,
    },
    before: OVERLAY_BEFORE,
    after: OVERLAY_AFTER,
    uncommitted: true,
  },
  {
    file: { path: "src/features/viewer/lib/diff-model.ts", status: "added", added: 9, removed: 0 },
    before: null,
    after: `export interface DiffTotals {\n  added: number;\n  removed: number;\n}\n\nexport function totals(files: { added?: number; removed?: number }[]): DiffTotals {\n  return files.reduce((t, f) => ({ added: t.added + (f.added ?? 0), removed: t.removed + (f.removed ?? 0) }), { added: 0, removed: 0 });\n}\n`,
    uncommitted: true,
  },
  {
    file: {
      path: "src/features/viewer/lib/row-format.ts",
      status: "modified",
      badges: ["rename"],
      renamedFrom: "src/features/viewer/lib/format.ts",
      added: 1,
      removed: 1,
    },
    before: `export const formatPath = (p: string) => p;\n`,
    after: `export const formatPath = (p: string) => p.replace(/^\\.\\//, "");\n`,
  },
  {
    file: {
      path: "src/features/viewer/lib/legacy-rail.ts",
      status: "deleted",
      added: 0,
      removed: 6,
    },
    before: `// The old flat rail.\nexport function legacyRail() {\n  return null;\n}\n\nexport default legacyRail;\n`,
    after: null,
  },
  {
    file: { path: "src-tauri/src/commands/baseline.rs", status: "modified", added: 7, removed: 2 },
    before: RUST_BEFORE,
    after: RUST_AFTER,
    uncommitted: true,
  },
  {
    file: {
      path: "src-tauri/src/commands/mod.rs",
      status: "modified",
      added: 1,
      removed: 0,
      badges: ["mode"],
    },
    before: `pub mod git_sync;\n`,
    after: `pub mod baseline;\npub mod git_sync;\n`,
  },
  {
    file: { path: "docs/source-diff.md", status: "modified", added: 4, removed: 3 },
    before: DOC_BEFORE,
    after: DOC_AFTER,
    uncommitted: true,
  },
  {
    file: { path: "assets/diff-hero.png", status: "added" },
    before: null,
    after: null,
    binary: true,
  },
  {
    file: { path: "src/generated/rows.ts", status: "added", added: 2400, removed: 0 },
    before: null,
    after: BIG_AFTER,
  },
  {
    file: {
      path: "scripts/release.sh",
      status: "modified",
      added: 0,
      removed: 0,
      badges: ["mode"],
    },
    before: `#!/bin/sh\nset -eu\npnpm build\n`,
    after: `#!/bin/sh\nset -eu\npnpm build\n`,
  },
  {
    file: { path: "package.json", status: "modified", added: 1, removed: 1 },
    before: `{\n  "name": "codesense",\n  "version": "0.0.18"\n}\n`,
    after: `{\n  "name": "codesense",\n  "version": "0.0.19"\n}\n`,
  },
];

type World = "ok" | "empty" | "error" | "slow" | "stale" | "large";

function world(search: string): World {
  const v = new URLSearchParams(search).get("diff");
  return v === "empty" || v === "error" || v === "slow" || v === "stale" || v === "large"
    ? v
    : "ok";
}

/** `diff=large`: the stress world — ~300 files across many folders plus one
 *  rewrite over 5k lines, built on first use so the normal worlds pay nothing. */
let largeFixture: FixtureFile[] | null = null;
function large(): FixtureFile[] {
  if (largeFixture) return largeFixture;
  const generated: FixtureFile[] = Array.from({ length: 296 }, (_, i) => {
    const dir = `src/features/area-${String(i % 24).padStart(2, "0")}/${i % 3 === 0 ? "lib" : "components"}`;
    const n = 20 + (i % 40);
    const before = lines(n, (j) => `export const value${j} = ${j};`);
    const after = lines(n, (j) =>
      j % 7 === 0 ? `export const value${j} = ${j} * 2;` : `export const value${j} = ${j};`,
    );
    const changed = Math.ceil(n / 7);
    return {
      file: {
        path: `${dir}/module-${i}.ts`,
        status: "modified" as const,
        added: changed,
        removed: changed,
      },
      before,
      after,
    };
  });
  const rewriteBefore = lines(5200, (i) => `  row(${i}, "label ${i}"),`);
  const rewriteAfter = lines(5400, (i) =>
    i % 9 === 0 ? `  row(${i}, "label ${i}", { pinned: true }),` : `  row(${i}, "label ${i}"),`,
  );
  generated.push({
    file: { path: "src/generated/table.ts", status: "modified", added: 800, removed: 600 },
    before: rewriteBefore,
    after: rewriteAfter,
  });
  largeFixture = [...FIXTURE, ...generated];
  return largeFixture;
}

function fixtureFor(search: string): FixtureFile[] {
  return world(search) === "large" ? large() : FIXTURE;
}

/** What `remote_source.rs` answers when the machine's terminal server predates
 *  the diff operations (`DIFF_NEEDS_SERVER_UPDATE`). */
const STALE_SERVER = {
  kind: "transportFailure",
  message:
    "This machine's terminal server is too old to diff its workspaces. Update the terminal server from Machines, then retry.",
};

function changeset(search: string, baseRef: unknown, remote = false): Promise<PublishFile[]> {
  const w = world(search);
  if (w === "error")
    return Promise.reject("fatal: not a git repository (or any of the parent directories)");
  if (w === "stale" && remote) return Promise.reject(STALE_SERVER);
  const files =
    w === "empty"
      ? []
      : fixtureFor(search)
          .filter((f) => baseRef !== "HEAD" || f.uncommitted)
          .map((f) => f.file);
  const delay = w === "slow" ? 1600 : 60;
  return new Promise((resolve) => setTimeout(() => resolve(files), delay));
}

function redline(search: string, args: Record<string, unknown>): FileRedline {
  const hit = fixtureFor(search).find((f) => f.file.path === args.filePath);
  if (!hit) throw new Error(`no change at ${String(args.filePath)}`);
  return {
    status: hit.file.status,
    binary: hit.binary ?? false,
    before: hit.before ?? "",
    after: hit.after ?? "",
  };
}

export function sourceDiffHandlers(search: string): HandlerRegistry {
  return {
    source_worktree_changeset: (args) => changeset(search, args.baseRef),
    source_file_redline: (args) => redline(search, args),
    source_remote_changeset: (args) => changeset(search, args.baseRef, true),
    source_remote_file_redline: (args) => redline(search, args),
    source_remote_watch_start: () => 1,
    source_remote_watch_stop: () => undefined,
    source_remote_tracked_count: () => 1200,
    lazy_attach_worktree_watch: () => 1,
    detach_worktree_watch: () => undefined,
    check_branch_exists: () => true,
    detect_tools: () => ({
      terminals: [],
      editors: [{ id: "cursor", label: "Cursor" }],
      default_terminal: "",
      default_editor: "cursor",
    }),
  };
}
