// SPDX-License-Identifier: MIT
import { fuzzyScore } from "@/shared/lib/fuzzy-match";

export interface SettingRowSpec {
  key: string;
  label: string;
  keywords: string[];
}

export interface RowMatchOptions {
  // Enables the fuzzy-score fallback on top of substring matching.
  // Defaults to `false` — long keyword lists let `fuzzyScore` walk
  // arbitrary chars and false-match unrelated queries (e.g. "theme"
  // walks t-h-e-m-e through a content-width keyword list). Opt in
  // per caller if fuzzy semantics are genuinely desired.
  fuzzy?: boolean;
}

export function rowMatches(
  spec: SettingRowSpec,
  query: string,
  opts: RowMatchOptions = {},
): boolean {
  if (!query) return true;
  const needle = query.trim().toLowerCase();
  if (!needle) return true;
  const haystack = [spec.label, ...spec.keywords].join(" ").toLowerCase();
  if (haystack.includes(needle)) return true;
  if (!opts.fuzzy) return false;
  return fuzzyScore(needle, haystack) > 0;
}
