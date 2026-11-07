// SPDX-License-Identifier: MIT
import { Fragment } from "react";
import { ChevronRight } from "lucide-react";

import { FileGlyph } from "./file-glyph";

/**
 * Where the cursor is: the file's folders, the file, then the enclosing code
 * scope (class › method) when the language has one. Folders are dimmed so the
 * file and scope — what changes as you move — read first.
 */
export function EditorBreadcrumbs({
  relPath,
  absPath,
  scope,
}: {
  relPath: string;
  absPath: string;
  scope: readonly string[] | null;
}) {
  const parts = relPath.split("/").filter(Boolean);
  const file = parts.pop() ?? relPath;
  return (
    <nav
      aria-label="Breadcrumb"
      data-testid="worktree-breadcrumbs"
      className="flex h-7 min-w-0 shrink-0 items-center gap-0.5 overflow-hidden border-b border-[var(--border)] bg-[var(--background)] px-3 text-[0.72rem] text-muted-foreground"
    >
      {parts.map((dir, i) => (
        <Fragment key={`${dir}-${i}`}>
          <span className="max-w-[10rem] shrink truncate">{dir}</span>
          <ChevronRight aria-hidden className="size-3 shrink-0 opacity-60" />
        </Fragment>
      ))}
      <span className="flex min-w-0 shrink-0 items-center gap-1 text-foreground" title={absPath}>
        <FileGlyph path={absPath} className="size-3" />
        <span className="truncate">{file}</span>
      </span>
      {scope?.map((s, i) => (
        <Fragment key={`${s}-${i}`}>
          <ChevronRight aria-hidden className="size-3 shrink-0 opacity-60" />
          <span className="min-w-0 truncate font-mono text-[0.7rem] text-foreground/85">{s}</span>
        </Fragment>
      ))}
    </nav>
  );
}
