// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { GitMerge, TriangleAlert } from "lucide-react";

import { CodeEditor, detectLanguageId } from "@/shared/components/code-editor";
import type { ResolverContext } from "@/shared/hooks/use-resolver-context";
import {
  listMergePreviewRepos,
  previewMergeMarkedFile,
  type MergePreviewRepo,
} from "@/tauri/commands";

/**
 * SW-CP-29: the read-only, repo-scoped deep-merge PREVIEW for a merge-marked
 * content file. Pick a target repo and see `merge(repo's current file ⊕ this
 * fragment)` — the exact text the projection wave would materialize there right
 * now (computed backend-side via the SAME primitive the wave uses, so preview ==
 * reality).
 *
 * Strictly read-only by construction: you edit the fragment, never the merged
 * view — an editable merged blob is ambiguous (which bytes are the fragment vs.
 * the repo's base?) and reverse-syncing it out would be a footgun. The merge is
 * inherently per-repo: each repo's own current file is the base, so three repos
 * yield three results — hence the required selector. Labeled an as-merged-NOW
 * snapshot, not a canonical file.
 */
type MergePreviewPanelProps = {
  /** Ready resolver context — the (orgSlug, teamName, codesenseHome, userId) the
   *  backend command needs. */
  ctx: ResolverContext;
  /** The merge-marked file's content-relative logical path (the marks-store key
   *  and the fragment identity). */
  logicalPath: string;
  /** Display name for the read-only editor's aria-label. */
  filename: string;
};

type ReposState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; repos: MergePreviewRepo[] };

type PreviewState =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; text: string; note: string };

function messageOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function MergePreviewPanel({
  ctx,
  logicalPath,
  filename,
}: MergePreviewPanelProps): React.ReactElement {
  const [repos, setRepos] = useState<ReposState>({ kind: "loading" });
  const [selected, setSelected] = useState<string>("");
  const [preview, setPreview] = useState<PreviewState>({ kind: "idle" });

  // Load the candidate repos once per (context). The list is team-wide and
  // independent of the open file, so `logicalPath` is deliberately NOT a
  // dependency — switching files does not re-fetch or reset the selection. On
  // reload the prior selection is kept if it still exists, else the first repo
  // is auto-picked so the panel shows a merge immediately (AC-1).
  useEffect(() => {
    let cancelled = false;
    // Load-on-change: show the "loading repos" state while the fetch is in
    // flight; the async result commits the real state (repos + selection).
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setRepos({ kind: "loading" });
    listMergePreviewRepos(ctx.codesenseHome, ctx.userId, ctx.orgSlug, ctx.teamName)
      .then((list) => {
        if (cancelled) return;
        setRepos({ kind: "ready", repos: list });
        setSelected((prev) =>
          list.some((r) => r.codeRepoPath === prev) ? prev : (list[0]?.codeRepoPath ?? ""),
        );
      })
      .catch((e) => {
        if (!cancelled) setRepos({ kind: "error", message: messageOf(e) });
      });
    return () => {
      cancelled = true;
    };
  }, [ctx.codesenseHome, ctx.userId, ctx.orgSlug, ctx.teamName]);

  // Compute the preview for the selected repo. Re-runs on repo switch (AC-4:
  // switching repos re-renders a different merged result when the bases differ).
  useEffect(() => {
    let cancelled = false;
    // Load-on-change: idle with no repo selected, else show the "computing"
    // state while the merge is in flight; the async result commits the merged
    // text (or an error).
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setPreview(selected ? { kind: "loading" } : { kind: "idle" });
    if (!selected) return;
    previewMergeMarkedFile(
      ctx.codesenseHome,
      ctx.userId,
      ctx.orgSlug,
      ctx.teamName,
      logicalPath,
      selected,
    )
      .then((result) => {
        if (!cancelled) setPreview({ kind: "ready", text: result.text, note: result.note });
      })
      .catch((e) => {
        if (!cancelled) setPreview({ kind: "error", message: messageOf(e) });
      });
    return () => {
      cancelled = true;
    };
  }, [ctx.codesenseHome, ctx.userId, ctx.orgSlug, ctx.teamName, logicalPath, selected]);

  if (repos.kind === "loading") {
    return (
      <div className="p-3 text-xs text-muted-foreground" data-testid="merge-preview-loading-repos">
        Loading target repos…
      </div>
    );
  }
  if (repos.kind === "error") {
    return (
      <div
        className="flex items-center gap-2 p-3 text-xs text-muted-foreground"
        data-testid="merge-preview-repos-error"
      >
        <TriangleAlert size={12} /> Couldn’t load target repos: {repos.message}
      </div>
    );
  }
  if (repos.repos.length === 0) {
    return (
      <div className="p-3 text-xs text-muted-foreground" data-testid="merge-preview-no-repos">
        No attached repos to preview against — attach a repo to see the merged result.
      </div>
    );
  }

  const selectedRepo = repos.repos.find((r) => r.codeRepoPath === selected);
  const repoLabel = selectedRepo ? `${selectedRepo.owner}/${selectedRepo.repo}` : "repo";

  return (
    <div className="flex h-full flex-col" data-testid="merge-preview-panel">
      {/* Read-only, repo-scoped, snapshot-in-time label + selector (AC-3/AC-5). */}
      <div
        className="flex flex-wrap items-center gap-2 border-b px-3 py-2 text-xs"
        style={{ borderColor: "var(--border)" }}
      >
        <GitMerge size={12} className="text-sky-500" />
        <span className="font-medium">Merged preview</span>
        <span className="text-muted-foreground">read-only · as a launch writes it into</span>
        <select
          className="rounded border bg-transparent px-1.5 py-0.5 text-xs"
          style={{ borderColor: "var(--border)" }}
          value={selected}
          onChange={(e) => setSelected(e.target.value)}
          aria-label="Preview target repo"
          data-testid="merge-preview-repo-select"
        >
          {repos.repos.map((r) => (
            <option key={r.codeRepoPath} value={r.codeRepoPath}>
              {r.owner}/{r.repo}
            </option>
          ))}
        </select>
        <span className="text-muted-foreground">· snapshot now</span>
      </div>

      <div className="relative flex-1 overflow-hidden">
        {preview.kind === "loading" && (
          <div className="p-3 text-xs text-muted-foreground" data-testid="merge-preview-computing">
            Computing merge…
          </div>
        )}
        {preview.kind === "error" && (
          <div
            className="flex items-center gap-2 p-3 text-xs text-muted-foreground"
            data-testid="merge-preview-error"
          >
            <TriangleAlert size={12} /> {preview.message}
          </div>
        )}
        {preview.kind === "ready" && (
          <div
            className="border-b px-3 py-1.5 text-xs text-muted-foreground"
            style={{ borderColor: "var(--border)" }}
            data-testid="merge-preview-outcome"
          >
            {preview.note}
          </div>
        )}
        {preview.kind === "ready" && (
          <CodeEditor
            value={preview.text}
            onChange={() => {}}
            readOnly
            path={logicalPath}
            language={detectLanguageId(logicalPath, preview.text)}
            height="100%"
            ariaLabel={`Read-only merged preview of ${filename} as merged into ${repoLabel}`}
          />
        )}
      </div>
    </div>
  );
}
