// SPDX-License-Identifier: MIT
/**
 * SW-VE-11: editor power-config affordances — the single source of truth for
 * resolving per-type editor settings (folding, whitespace, tab size, prose font,
 * plus the SW-VE-8 line-wrap + gutter) into the concrete config every
 * `<CodeEditor>` mount feeds CM6.
 *
 * Why one module: SW-VE-8 resolved wrap + gutter inline in the context inset,
 * and the source-worktree inset never got them at all — the three editor mounts
 * drifted. Because these settings are GLOBAL, "tab size = 2" must mean the same
 * thing in every in-app editor. Resolving the whole set in ONE pure function
 * (consumed by every mount) makes the affordances impossible to drift — the
 * "wrap + gutter never drift" principle from `language-detection.ts`, extended.
 */
import { isCodeLanguagePath } from "./language-detection";

// ──────────────────────────────────────────────────────────────────────
// Per-type tri-state core (shared by line-wrap AND folding)
// ──────────────────────────────────────────────────────────────────────

/** A setting that is either forced on/off or derives from the file type. */
export type EditorTriState = "auto" | "on" | "off";

/**
 * Resolve a per-type tri-state setting to an effective boolean.
 *   - `"on"`  → always true
 *   - `"off"` → always false
 *   - `"auto"` → the per-type default: `autoOn: "code"` is on for code / off for
 *     prose; `autoOn: "prose"` is the inverse.
 *
 * TOTAL + fail-safe: any unexpected value (corrupted persisted state, a caller
 * that skipped coercion) resolves to the `auto` default rather than returning
 * `undefined`, so consumers never crash on a bad setting. This is the shared
 * core `resolveEditorLineWrap` (prose-auto) and `resolveEditorFolding`
 * (code-auto) both delegate to — one implementation, no drift.
 */
export function resolvePerTypeTriState(
  setting: EditorTriState,
  isCode: boolean,
  autoOn: "code" | "prose",
): boolean {
  if (setting === "on") return true;
  if (setting === "off") return false;
  return autoOn === "code" ? isCode : !isCode;
}

// ──────────────────────────────────────────────────────────────────────
// Code folding (AC-1) — per-type tri-state, `auto` = code folds / prose doesn't
// ──────────────────────────────────────────────────────────────────────

export type EditorFolding = EditorTriState;

/** Default: `auto` — fold gutter on code, off for prose (mirrors line-wrap's
 *  auto philosophy, polarity inverted). */
export const DEFAULT_EDITOR_FOLDING: EditorFolding = "auto";

export function resolveEditorFolding(setting: EditorFolding, isCode: boolean): boolean {
  return resolvePerTypeTriState(setting, isCode, "code");
}

// ──────────────────────────────────────────────────────────────────────
// Tab size / indent unit (AC-3)
// ──────────────────────────────────────────────────────────────────────

export type EditorTabSize = 2 | 4 | 8;

export const EDITOR_TAB_SIZES: readonly EditorTabSize[] = [2, 4, 8];

/** Default 2 — codesense content is predominantly 2-space (yaml/md/ts). */
export const DEFAULT_EDITOR_TAB_SIZE: EditorTabSize = 2;

/** Narrow any persisted/unknown value to a valid tab size (else the default),
 *  so the setting stays a real enum member for the radio + CM6 facets. */
export function coerceTabSize(value: unknown): EditorTabSize {
  return value === 2 || value === 4 || value === 8 ? value : DEFAULT_EDITOR_TAB_SIZE;
}

// ──────────────────────────────────────────────────────────────────────
// Prose reading-font (AC-4)
// ──────────────────────────────────────────────────────────────────────

/** The editor's default monospace stack. Single source of truth — previously
 *  copy-pasted into `use-code-editor.ts` and `use-code-merge-view.ts`. */
export const MONO_FONT_STACK = "'MesloLGS NF', ui-monospace, SFMono-Regular, Menlo, monospace";

/** Proportional reading stack for prose editing (iA-Writer-style, system-safe —
 *  no bundled font). Applied to prose files only when `editorProseFont` is on. */
export const PROSE_FONT_STACK = 'ui-serif, Georgia, Cambria, "Times New Roman", serif';

/**
 * Resolve the editor font-family. Prose files get the reading stack ONLY when
 * the setting is on; code files are ALWAYS monospace (tab alignment matters).
 */
export function resolveEditorFontFamily(proseFontOn: boolean, isCode: boolean): string {
  return proseFontOn && !isCode ? PROSE_FONT_STACK : MONO_FONT_STACK;
}

// ──────────────────────────────────────────────────────────────────────
// Aggregate resolver — the one call every editor mount makes
// ──────────────────────────────────────────────────────────────────────

/** The subset of `ViewerSettings` the affordance resolver reads. Declared
 *  structurally (not imported from the store) to keep this a leaf module with
 *  no import cycle; `ViewerSettings` satisfies it by shape. */
export interface EditorAffordanceSettings {
  editorLineWrap: EditorTriState;
  editorFolding: EditorFolding;
  editorShowWhitespace: boolean;
  editorTabSize: EditorTabSize;
  editorProseFont: boolean;
}

/** The concrete, per-type config a `<CodeEditor>` mount feeds CM6. */
export interface ResolvedEditorAffordances {
  gutters: boolean;
  lineWrap: boolean;
  folding: boolean;
  showWhitespace: boolean;
  tabSize: EditorTabSize;
  fontFamily: string;
}

/**
 * Resolve ALL per-type editor affordances for one file path from the global
 * settings. Pure + total — the single place per-type policy lives, so every
 * editor mount stays consistent. `gutters`/`lineWrap` reproduce SW-VE-8's
 * per-type defaults (code: gutter on, no wrap; prose: gutter off, wrap on).
 */
export function resolveEditorAffordances(
  settings: EditorAffordanceSettings,
  path: string,
): ResolvedEditorAffordances {
  const isCode = isCodeLanguagePath(path);
  return {
    gutters: isCode,
    lineWrap: resolvePerTypeTriState(settings.editorLineWrap, isCode, "prose"),
    folding: resolveEditorFolding(settings.editorFolding, isCode),
    showWhitespace: settings.editorShowWhitespace,
    // Coerce here too so the resolver is genuinely TOTAL: a bad tab size can
    // reach the store UNcoerced via a direct write (e.g. the MCP
    // `set_viewer_state` path bypasses the load-time coercion), and an unclamped
    // value would crash the editor at `" ".repeat(tabSize)`. This is the single
    // choke point every editor mount reads, so one coerce closes every path.
    tabSize: coerceTabSize(settings.editorTabSize),
    fontFamily: resolveEditorFontFamily(settings.editorProseFont, isCode),
  };
}
