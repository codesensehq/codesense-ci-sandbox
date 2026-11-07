// SPDX-License-Identifier: MIT
import { fileChangeAffects } from "@/shared/lib/file-change-affects";
import { useEffect, useRef, useState } from "react";

import {
  CodeEditor,
  detectLanguageId,
  useEditorAffordances,
} from "@/shared/components/code-editor";
import { fileContentCache } from "@/shared/lib/file-content-cache";
import { FileDeletedPlaceholder } from "@/shared/components/file-deleted-placeholder";
import { closeActiveFileTab } from "@/shared/lib/close-active-file-tab";
import { isFileMissingError } from "@/shared/lib/file-missing-error";
import { logger } from "@/shared/lib/logger";
import type { CommonViewerProps } from "@/shared/lib/viewer-file-type";
import { readFile } from "@/tauri/commands";

import { isTooLargeToView, looksBinary, utf8ByteLength } from "../lib/worktree-file-view";
import { FileWarningPlaceholder } from "./file-warning-placeholder";

/**
 * SW-VE-2: the `code` render case in {@link renderViewer} — a read-only,
 * syntax-highlighted view of a code/config/data file (yaml, toml, json, xml,
 * ini, env, csv, and code, plus the plain-text catch-all). It reuses the
 * source-worktree CM6 `CodeEditor` in read-only mode (the `CodeDiffReview`
 * pattern — no save/dirty machinery); it does NOT fork a second editor.
 *
 * The wrapper — not the path gate — is the final text-vs-binary authority
 * (README §Guardrails "the viewer decides render vs binary-placeholder"):
 * after a non-blocking, cached read it gates a binary (NUL sniff) or oversized
 * file to a placeholder. The shared `renderViewer` does NOT pre-gate on
 * `isPreviewableFilePath`, so these post-read guards are load-bearing here,
 * not just belt-and-suspenders.
 */
type CodeFileViewerProps = CommonViewerProps & {
  filePath: string;
  /** SW-FX-5 parity: the watcher's changed-file set — skip the re-read when a
   *  sibling-file tick bumped `refreshKey` but this file didn't change. */
  changedPaths?: Set<string> | null;
};

type LoadState = "loading" | "ready" | "binary" | "too-large" | "error" | "removed";

const noop = (): void => {};

export function CodeFileViewer({
  filePath,
  refreshKey,
  changedPaths,
}: CodeFileViewerProps): React.ReactElement {
  const [content, setContent] = useState("");
  const [state, setState] = useState<LoadState>("loading");
  // SW-VE-11: display-meaningful affordances even in the read-only viewer —
  // folding (collapse regions while reading), visible whitespace, and tab-size
  // rendering. Wrap/gutter/prose-font stay at the read-only viewer's defaults
  // (this is a code-only viewer).
  const affordances = useEditorAffordances(filePath);
  // Last filePath whose content actually landed on THIS instance — gates the
  // loading flip so a same-file refresh re-read doesn't flash a skeleton over
  // already-rendered content.
  const lastLoadedPath = useRef<string | null>(null);

  useEffect(() => {
    let stale = false;
    const currentRefreshKey = refreshKey ?? 0;

    // Classify + commit resolved text. Binary/too-large win over render so a
    // listed-but-unrenderable file shows a placeholder, never a raw dump/hang.
    const commit = (text: string): void => {
      if (stale) return;
      if (looksBinary(text)) {
        setState("binary");
        return;
      }
      if (isTooLargeToView(utf8ByteLength(text))) {
        setState("too-large");
        return;
      }
      setContent(text);
      lastLoadedPath.current = filePath;
      setState("ready");
    };

    const cached = fileContentCache.get(filePath);
    if (cached && cached.refreshKey === currentRefreshKey) {
      commit(cached.content);
      return;
    }
    // Sibling-file tick: refreshKey bumped but this file is not in the changed
    // set — forward the cache token and reuse cached content (no re-read).
    if (!fileChangeAffects(changedPaths, filePath) && cached) {
      fileContentCache.set(filePath, cached.content, currentRefreshKey);
      commit(cached.content);
      return;
    }

    if (lastLoadedPath.current !== filePath) {
      // First/changed load: show the skeleton AND clear stale content so a reused
      // instance never renders the previous file's text under the new file's
      // language. The caller keys by path (fresh instance per file), so this is
      // defensive — it makes the component self-safe if that invariant changes.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setState("loading");
      setContent("");
    }
    readFile(filePath)
      .then((text) => {
        if (stale) return;
        fileContentCache.set(filePath, text, currentRefreshKey);
        commit(text);
      })
      .catch((e) => {
        if (stale) return;
        // SW-VE-4: an agent deleted the file underneath the open read-only view —
        // self-heal to the shared "was removed" empty state (mirrors the editor +
        // markdown/html viewers), not a generic "Couldn't read file" error.
        if (isFileMissingError(e)) {
          logger.info("viewer", `code view file removed: ${filePath}`);
          setState("removed");
          return;
        }
        logger.error("viewer", `code view read failed: ${filePath} — ${e}`);
        setState("error");
      });

    return () => {
      stale = true;
    };
  }, [filePath, refreshKey, changedPaths]);

  const filename = filePath.split("/").pop() ?? filePath;

  if (state === "binary") {
    return (
      <FileWarningPlaceholder
        message="Binary file — no preview"
        filename={filename}
        testId="code-view-binary"
      />
    );
  }
  if (state === "too-large") {
    return (
      <FileWarningPlaceholder
        message="File too large to view"
        filename={filename}
        testId="code-view-too-large"
      />
    );
  }
  if (state === "removed") {
    return (
      <FileDeletedPlaceholder fileName={filename} onClose={() => closeActiveFileTab(filePath)} />
    );
  }
  if (state === "error") {
    return (
      <FileWarningPlaceholder
        message="Couldn't read file"
        filename={filename}
        testId="code-view-error"
      />
    );
  }

  // loading + ready both mount the editor: CM6 shows its own "Loading editor…"
  // skeleton while its lazy chunk loads (in parallel with the file read), and
  // `value` updates in place from "" → content when the read lands. `readOnly`
  // blocks keystrokes (proven in CodeEditor.test.tsx).
  //
  // `language` is computed HERE (not left to CodeEditor's "auto") because
  // CodeEditor memoizes detection on [language, path] and deliberately excludes
  // `value` (an editable-editor keystroke optimisation). Content arrives async
  // AFTER mount, so an extensionless shebang script (`#!/bin/bash` with no ext)
  // would otherwise stay "plain": passing the detected id makes `language` change
  // plain → shell once the read lands, re-highlighting it (AC-3).
  return (
    <CodeEditor
      value={content}
      onChange={noop}
      readOnly
      path={filePath}
      language={detectLanguageId(filePath, content)}
      folding={affordances.folding}
      showWhitespace={affordances.showWhitespace}
      tabSize={affordances.tabSize}
      height="100%"
      ariaLabel={`Read-only view of ${filename}`}
    />
  );
}
