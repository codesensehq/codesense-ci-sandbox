// SPDX-License-Identifier: MIT
import type { Tool } from "@/tauri/commands";

const GENERIC_LABEL = "Open in editor";

/**
 * SLW-WDE-5 §B — human label for the "open in external editor" control.
 *
 * `editor` is the persisted settings *id* ("code"/"cursor"/"custom"/""), NOT a
 * label — the friendly name ("VS Code") lives only in the async `detectTools()`
 * result. This pure helper derives the control label and degrades to a generic
 * string until detection resolves or when the editor is unknown/custom-without-
 * command, so it never blocks render and never invents a name the settings can't
 * back. Disambiguates from WDE-1's in-app "Open editor" control (§A) by always
 * reading "Open in <editor>" when a name is known.
 */
export function editorLaunchLabel(
  editor: string,
  editorCustomCmd: string,
  detectedEditors: Tool[],
): string {
  if (!editor) return GENERIC_LABEL;
  if (editor === "custom") {
    const cmd = editorCustomCmd.trim();
    return cmd ? `Open in ${cmd}` : GENERIC_LABEL;
  }
  const match = detectedEditors.find((t) => t.id === editor);
  return match ? `Open in ${match.label}` : GENERIC_LABEL;
}
