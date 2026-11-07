// SPDX-License-Identifier: MIT
/**
 * The slice of the viewer store the ⌘P quick-open selection needs. Kept
 * intentionally narrow — it exposes only the file-peek surface (open a file).
 * It deliberately has NO story-workspace orchestrator, so
 * the quick-open path structurally cannot boot a CLI/terminal workspace.
 */
export interface QuickOpenSelectStore {
  openFile: (teamKey: string, repoPath: string, filePath: string) => void;
}

/**
 * SW-FX-49: handle a ⌘P quick-open file selection.
 *
 * Opens the picked file. `openFile` itself reveals the viewer when the team is in
 * console (terminal-only) mode (SW-LN-B38), so the opened file is actually visible
 * instead of landing behind the console. That's the whole behavior: ⌘P is a fast
 * *peek*.
 *
 * It intentionally has no workspace side-effect. Opening a story's CLI/terminal
 * workspace is a separate, deliberate gesture — the workspace palette (⌘K,
 * `nav.open-workspace`). Previously this path also called the story-workspace
 * orchestrator for story files, so merely peeking at a story via ⌘P booted its
 * whole terminal workspace. Extracting the logic here (with a store slice that
 * has no orchestrator) makes that regression impossible by construction.
 */
export function handleQuickOpenFileSelect(
  store: QuickOpenSelectStore,
  teamKey: string,
  repoPath: string,
  fullPath: string,
): void {
  const source = resolveFileSource(fullPath);
  store.openFile(
    teamKey,
    source.source.kind === "private" ? source.contentRoot : repoPath,
    fullPath,
  );
}
import { resolveFileSource } from "@/shared/lib/file-source";
