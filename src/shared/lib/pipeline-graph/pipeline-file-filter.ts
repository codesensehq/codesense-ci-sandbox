// SPDX-License-Identifier: MIT
/**
 * Mirrors the Rust scanner's pipeline-file filter.
 *
 * A "pipeline file" is a file with a recognized pipeline extension under the
 * pipeline root that the scanner emits as a `StoryRef`. README and index files
 * (case-insensitive stem, any pipeline extension) are excluded — they're
 * treated as main docs, not stories.
 *
 * Keep this in sync with `is_pipeline_document()` in `pipeline_scanner.rs`
 * and `PIPELINE_EXTENSIONS` in `pipeline_format/mod.rs`. The two
 * implementations are deliberately duplicated rather than shared via an
 * RPC: frontend consumers (`revealOpenedFile`, future nav helpers) need
 * this answer synchronously without an IPC round-trip.
 */

/** Extensions eligible as pipeline documents. Independent from VIEWER_EXTENSIONS
 *  — adding a viewer-only format must not leak into the pipeline scanner.
 *  Keep in sync with PIPELINE_EXTENSIONS in pipeline_format/mod.rs. */
export const PIPELINE_EXTENSIONS: ReadonlySet<string> = new Set(["md", "html", "htm"]);

export function isPipelineFileName(fileName: string): boolean {
  const lower = fileName.toLowerCase();
  const dotIdx = lower.lastIndexOf(".");
  if (dotIdx < 0) return false;
  const ext = lower.slice(dotIdx + 1);
  if (!PIPELINE_EXTENSIONS.has(ext)) return false;
  const stem = lower.slice(0, dotIdx);
  if (stem === "readme" || stem === "index") return false;
  return true;
}
