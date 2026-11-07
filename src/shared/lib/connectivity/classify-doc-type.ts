// SPDX-License-Identifier: MIT
import { DOC_TYPE_ICON } from "../pipeline-graph/doc-type-icon";

/**
 * SW-DX-8 — map a context link to a `DOC_TYPE_ICON` bucket key (AC-7). A story
 * target carries its own `documentType` (story·bug·spike·doc·retro); a non-story
 * file (plain doc / binary leaf) is classified by extension. Every return value
 * is a KEY of the shared `DOC_TYPE_ICON` map — so the connectivity chip shows
 * the IDENTICAL icon the pipeline popover does (no second classifier).
 */

/** Extension → `DOC_TYPE_ICON` key for non-story files. */
const EXT_TO_KEY: Record<string, string> = {
  md: "doc",
  markdown: "doc",
  html: "doc",
  htm: "doc",
  txt: "doc",
  pdf: "doc",
  png: "image",
  jpg: "image",
  jpeg: "image",
  gif: "image",
  webp: "image",
  svg: "image",
  excalidraw: "canvas",
};

export function classifyDocType(fileName: string, documentType?: string | null): string {
  if (documentType) {
    const key = documentType.trim().toLowerCase();
    if (Object.prototype.hasOwnProperty.call(DOC_TYPE_ICON, key)) return key;
  }
  const dot = fileName.lastIndexOf(".");
  const ext = dot >= 0 ? fileName.slice(dot + 1).toLowerCase() : "";
  const byExt = EXT_TO_KEY[ext];
  if (byExt) return byExt;
  return "doc";
}
