// SPDX-License-Identifier: MIT
/**
 * The final segment of a `/`-delimited path (after the last slash), tolerant of
 * trailing slashes (`a/b/` → `b`). The single shared implementation — feature
 * modules import this rather than re-declaring their own `baseName`.
 */
export function basename(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  const idx = trimmed.lastIndexOf("/");
  return idx >= 0 ? trimmed.slice(idx + 1) : trimmed;
}

/**
 * The directory portion of a `/`-delimited path (everything before the last
 * slash). Mirrors POSIX `dirname` at the root boundary: no slash → `""`, a
 * single leading slash → `"/"`. The single shared implementation — modules
 * import this rather than re-declaring their own `dirname`.
 */
export function dirname(path: string): string {
  const i = path.lastIndexOf("/");
  if (i < 0) return "";
  if (i === 0) return "/";
  return path.slice(0, i);
}
