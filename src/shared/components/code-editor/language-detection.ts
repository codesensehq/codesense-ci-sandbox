// SPDX-License-Identifier: MIT
import type { Extension } from "@codemirror/state";
import type { CmModule, LanguageId } from "./cm-loader";

// L-7 patch: shebang regex uses an explicit interpreter whitelist instead
// of the prior alternation `(?:ba|z|k|c|tc|d|a)?sh\b`. The old form
// matched bogus prefixes like `ash`/`dsh` and missed `fish`. Anchors at
// the end of the shebang path or env-arg position. Examples it matches:
//   #!/bin/sh
//   #!/bin/bash
//   #!/usr/bin/env zsh
//   #!/usr/bin/env -S zsh
//   #!/path/to/my-bash
// Examples it does NOT match (correctly):
//   #!/usr/bin/env python3
//   #!/usr/bin/env -S python3
const SHELL_INTERPRETERS = "bash|sh|zsh|ksh|csh|tcsh|dash|ash|fish|nu";
const SHELL_SHEBANG_RE = new RegExp(
  `^#!(?:\\S*/)?(?:env\\s+(?:-S\\s+)?)?(?:[\\w-]*-)?(?:${SHELL_INTERPRETERS})\\b`,
);

/**
 * Pure path/content → language-id mapping. Shebang on the first line of
 * `content` overrides the extension mapping. Returns `"plain"` when no
 * language extension should be applied.
 */
// Extension → language. One table so detection, the status-bar label and the
// extension builder cannot disagree about which languages exist.
const LANGUAGE_BY_EXTENSION: Readonly<Record<string, LanguageId>> = {
  md: "markdown",
  markdown: "markdown",
  mdx: "markdown",
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  cts: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  json: "json",
  jsonc: "json",
  json5: "json",
  yaml: "yaml",
  yml: "yaml",
  py: "python",
  pyi: "python",
  sh: "shell",
  bash: "shell",
  zsh: "shell",
  rs: "rust",
  xml: "xml",
  svg: "xml",
  plist: "xml",
  css: "css",
  scss: "css",
  less: "css",
  html: "html",
  htm: "html",
  vue: "html",
  svelte: "html",
  go: "go",
  toml: "toml",
  sql: "sql",
  c: "c",
  h: "c",
  cc: "cpp",
  cpp: "cpp",
  cxx: "cpp",
  hpp: "cpp",
  hh: "cpp",
  java: "java",
  kt: "kotlin",
  kts: "kotlin",
  cs: "csharp",
  swift: "swift",
  rb: "ruby",
  lua: "lua",
  diff: "diff",
  patch: "diff",
  ini: "properties",
  cfg: "properties",
  conf: "properties",
  properties: "properties",
  env: "properties",
};

// Whole-filename matches for files conventionally named without an extension.
const LANGUAGE_BY_FILENAME: Readonly<Record<string, LanguageId>> = {
  dockerfile: "dockerfile",
  containerfile: "dockerfile",
  gemfile: "ruby",
  rakefile: "ruby",
  ".bashrc": "shell",
  ".zshrc": "shell",
  ".profile": "shell",
  ".env": "properties",
  ".editorconfig": "properties",
  ".gitconfig": "properties",
  "cargo.lock": "toml",
};

/**
 * Pure path/content → language-id mapping. Shebang on the first line of
 * `content` overrides the extension mapping. Returns `"plain"` when no
 * language extension should be applied.
 */
export function detectLanguageId(path?: string, content?: string): LanguageId {
  if (content) {
    const firstLine = content.split("\n", 1)[0] ?? "";
    if (SHELL_SHEBANG_RE.test(firstLine)) return "shell";
  }
  if (!path) return "plain";
  const name = (path.slice(path.lastIndexOf("/") + 1) || path).toLowerCase();
  // Own keys only: a file named `constructor` or `x.toString` must not resolve to
  // an Object.prototype member.
  const byName = Object.prototype.hasOwnProperty.call(LANGUAGE_BY_FILENAME, name)
    ? LANGUAGE_BY_FILENAME[name]
    : undefined;
  if (byName) return byName;
  if (name.startsWith("dockerfile.") || name.endsWith(".dockerfile")) return "dockerfile";
  const dot = name.lastIndexOf(".");
  if (dot <= 0 && !name.startsWith(".")) return "plain";
  const ext = name.slice(dot + 1);
  return Object.prototype.hasOwnProperty.call(LANGUAGE_BY_EXTENSION, ext)
    ? (LANGUAGE_BY_EXTENSION[ext] ?? "plain")
    : "plain";
}

const LANGUAGE_LABELS: Readonly<Record<LanguageId, string>> = {
  markdown: "Markdown",
  typescript: "TypeScript",
  javascript: "JavaScript",
  json: "JSON",
  yaml: "YAML",
  python: "Python",
  shell: "Shell",
  rust: "Rust",
  xml: "XML",
  css: "CSS",
  html: "HTML",
  go: "Go",
  toml: "TOML",
  sql: "SQL",
  c: "C",
  cpp: "C++",
  java: "Java",
  kotlin: "Kotlin",
  csharp: "C#",
  swift: "Swift",
  ruby: "Ruby",
  lua: "Lua",
  dockerfile: "Dockerfile",
  diff: "Diff",
  properties: "Properties",
  plain: "Plain Text",
};

/** Human label for a language id, as the editor's status bar shows it. */
export function languageLabel(langId: LanguageId): string {
  return LANGUAGE_LABELS[langId];
}

/**
 * SW-VE-8: prose-vs-code classification driving the editor's per-type defaults
 * (line wrapping ON + gutter OFF for prose; the inverse for code). Prose is
 * `markdown` and `plain`/unknown-text; everything else (including `xml` and
 * `html`) is code. Keep in one place so wrap + gutter never drift.
 */
export function isProseLanguage(langId: LanguageId): boolean {
  return langId === "markdown" || langId === "plain";
}

/** SW-VE-8: inverse of {@link isProseLanguage}. */
export function isCodeLanguage(langId: LanguageId): boolean {
  return !isProseLanguage(langId);
}

/**
 * SW-VE-8: classify a file PATH as code (vs prose) for the editor's per-type
 * wrap/gutter defaults. Prose = markdown + plain/unknown-text (wrap on, gutter
 * off); code = real code langs and markup. Self-contained + pure so wrap and
 * gutter never drift.
 */
export function isCodeLanguagePath(path: string): boolean {
  return isCodeLanguage(detectLanguageId(path));
}

/**
 * Build the CM6 language extension for a given language id, using factories
 * from the loaded `CmModule`. Returns `null` for `"plain"` (no extension to
 * apply).
 */
export function buildLanguageExtension(cm: CmModule, langId: LanguageId): Extension | null {
  if (langId === "plain") return null;
  // Guarded: an id with no factory (a new language not wired into the loader)
  // falls back to plain text instead of throwing at mount.
  const factory = cm.languages[langId] as (() => Extension) | undefined;
  return factory ? factory() : null;
}

/**
 * Convenience: detect + build in one call. Returns `null` for plain text.
 */
export function detectLanguage(cm: CmModule, path?: string, content?: string): Extension | null {
  return buildLanguageExtension(cm, detectLanguageId(path, content));
}
