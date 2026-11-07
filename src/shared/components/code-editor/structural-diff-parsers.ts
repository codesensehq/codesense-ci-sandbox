// SPDX-License-Identifier: MIT
/**
 * SLW-DV-8b (AC-3) — the Lezer parsers the structural diff needs, isolated here
 * so `structural-diff.worker.ts` can pull the grammar bundles WITHOUT the pure
 * engine (`structural-diff.ts`) ever importing a language pack. Lezer trees are
 * non-cloneable, so parsing must happen inside the worker (never posted across the
 * boundary). These are the same grammars `cm-loader.ts` already ships — no new
 * dependency. The parser variant matches `cm-loader`'s language factories:
 * `typescript` = tsx (covers .ts + .tsx), `javascript` = jsx (covers .js + .jsx).
 */

import { jsxLanguage, tsxLanguage } from "@codemirror/lang-javascript";
import { jsonLanguage } from "@codemirror/lang-json";
import { pythonLanguage } from "@codemirror/lang-python";
import { yamlLanguage } from "@codemirror/lang-yaml";
import type { Tree } from "@lezer/common";

import type { LanguageId } from "./cm-loader";

/**
 * Parse `text` into a Lezer tree for the given language, or `null` when the
 * language has no structural parser (a flat `StreamLanguage`, plain text, or
 * markdown). Callers treat `null` as "not structural → line diff".
 */
export function parseStructural(text: string, language: LanguageId): Tree | null {
  switch (language) {
    case "typescript":
      return tsxLanguage.parser.parse(text);
    case "javascript":
      return jsxLanguage.parser.parse(text);
    case "json":
      return jsonLanguage.parser.parse(text);
    case "yaml":
      return yamlLanguage.parser.parse(text);
    case "python":
      return pythonLanguage.parser.parse(text);
    default:
      return null;
  }
}
