// SPDX-License-Identifier: MIT
/**
 * True when a read/IO error means the file is GONE (deleted/renamed) rather than
 * a transient or permission failure — the signal the viewers use to self-heal to
 * a "file was removed" state instead of a generic error. Sniffs the message for
 * the two shapes the Tauri fs layer surfaces (`std::io` ENOENT phrasing and the
 * raw OS code); accepts a thrown `unknown` or an already-extracted message string
 * so every caller (the editor hook, the code viewer, the markdown viewer) agrees
 * on one definition.
 */
export function isFileMissingError(err: unknown): boolean {
  const msg = err instanceof Error ? err.message : String(err);
  // `os error 2` exactly: a bare substring would also match 20–29 (e.g. 24,
  // "Too many open files"), which are not a missing file.
  return msg.includes("No such file") || /\bos error 2\b/.test(msg);
}
