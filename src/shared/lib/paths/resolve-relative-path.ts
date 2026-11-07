// SPDX-License-Identifier: MIT
/**
 * Resolve an absolute `base` directory against a relative `rel` reference,
 * collapsing `.` / `..` / empty segments. A naive `path.resolve` for the
 * browser/worker (no Node `path` module): pure, allocation-light, and safe
 * against traversal — `..` never pops above the root.
 *
 * SW-DX-2 (AC-17): extracted verbatim from the private copy in
 * `pipeline-graph/derived-tree-to-react-flow.ts` so the pipeline transform AND
 * the doc-query backlink index resolve paths through ONE implementation and
 * cannot diverge. SW-CV-1's context graph consumes this same helper.
 *
 * @param base absolute directory path (e.g. a story's `dirname(filePath)`)
 * @param rel  relative reference (e.g. `"../workspaces/W-5-foo.md"`)
 * @returns the resolved absolute path
 */
export function resolveRelativePath(base: string, rel: string): string {
  const parts = base.split("/").filter(Boolean);
  for (const seg of rel.split("/")) {
    if (seg === "..") {
      // Floor: never pop above the root to prevent path traversal.
      if (parts.length > 0) parts.pop();
    } else if (seg !== "." && seg !== "") {
      parts.push(seg);
    }
  }
  return (base.startsWith("/") ? "/" : "") + parts.join("/");
}
