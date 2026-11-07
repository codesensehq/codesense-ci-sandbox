// SPDX-License-Identifier: MIT
/**
 * SW-VE-8: editor line-wrapping policy.
 *
 * Prose (markdown / plain text) should soft-wrap by default — a paragraph that
 * horizontal-scrolls is unusable (cf. Obsidian / iA Writer / Typora). Code
 * should NOT wrap by default (cf. VS Code code defaults). One global setting
 * overrides the per-type default, mirroring VS Code's `editor.wordWrap`.
 */
import { resolvePerTypeTriState, type EditorTriState } from "./editor-affordances";

export type EditorLineWrap = EditorTriState;

/** The default when a user has never touched the setting. */
export const DEFAULT_EDITOR_LINE_WRAP: EditorLineWrap = "auto";

/**
 * Resolve the effective wrap boolean for one editor from the global
 * `editorLineWrap` setting and whether the open file is code (vs prose):
 *   - `"auto"` → wrap prose, don't wrap code (the per-type default)
 *   - `"on"`   → always wrap
 *   - `"off"`  → never wrap
 *
 * `isCode` is derived from the file PATH via `isCodeLanguagePath` in
 * `language-detection.ts` (the classifier the editor actually feeds in): prose =
 * markdown + plain/unknown-text; code = real code langs AND markup (html).
 *
 * SW-VE-11: this is now a thin wrapper over the shared `resolvePerTypeTriState`
 * core (prose-auto polarity), which code folding also uses (code-auto) — one
 * tri-state implementation, no drift. Stays TOTAL/fail-safe (any unexpected
 * value → the per-type default, never `undefined`).
 */
export function resolveEditorLineWrap(setting: EditorLineWrap, isCode: boolean): boolean {
  return resolvePerTypeTriState(setting, isCode, "prose");
}
