// SPDX-License-Identifier: MIT
/**
 * Path normalization helpers for pipeline configuration.
 *
 * These are the single source of truth for scope-path and mainDoc
 * normalization rules on the TypeScript side. Every call site that stores
 * or consumes a scopePath / mainDoc value MUST call one of these —
 * no inline string manipulation elsewhere. (SW-FX-56: `normalizePipelineRoot`
 * removed — its only caller, `setPipelineRoot`, was deleted.)
 *
 * Rules (identical across both helpers):
 *   - Trim whitespace
 *   - Strip leading and trailing slashes (both `/` and `\`)
 *   - Reject any value containing `..` (throws; callers handle)
 *   - Empty string is a valid output (means "no root" or "at the root")
 *
 * keep rules in sync with normalize_pipeline_root / normalize_scope_path
 * in src-tauri/src/commands/pipeline_scanner.rs
 */

class PipelinePathError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "PipelinePathError";
  }
}

function normalize(value: string, fieldName: string): string {
  const trimmed = value.trim();
  // Strip leading and trailing `/` and `\` repeatedly
  const stripped = trimmed.replace(/^[/\\]+|[/\\]+$/g, "");
  if (stripped.length === 0) return "";
  // Reject path traversal — split on both separators and check each component
  const parts = stripped.split(/[/\\]/);
  for (const part of parts) {
    if (part === "..") {
      throw new PipelinePathError(`${fieldName} must not contain '..' — got: ${value}`);
    }
  }
  return stripped;
}

/**
 * Normalize a pipeline scope path (relative to the scan base).
 * Throws PipelinePathError on `..`.
 */
export function normalizeScopePath(value: string): string {
  return normalize(value, "scopePath");
}

/**
 * Normalize a mainDoc setting value (filename like "Epic.md").
 * Throws PipelinePathError on `..`.
 */
export function normalizeMainDoc(value: string): string {
  return normalize(value, "mainDoc");
}

export { PipelinePathError };
