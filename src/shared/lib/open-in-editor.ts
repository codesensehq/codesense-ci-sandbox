// SPDX-License-Identifier: MIT
import { openEditor } from "@/tauri/commands";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { toast } from "sonner";
import { getErrorMessage } from "@/shared/lib/error-message";

/**
 * Open the configured external editor at the given working directory.
 * Reads editor settings from the viewer store, guards on editor being
 * configured, and toasts on error. For use when the cwd is already
 * known (e.g. from a workspace); use ensureWorktree first when the
 * worktree may not exist yet.
 */
export function openInEditorFromCwd(cwd: string): void {
  const { editor, editorCustomCmd } = useViewerStore.getState().settings;
  if (!editor) {
    toast.error("No editor configured — set one in Settings");
    return;
  }
  openEditor(editor, editorCustomCmd, cwd).catch((err: unknown) => {
    toast.error(`Failed to open editor: ${getErrorMessage(err)}`);
  });
}
