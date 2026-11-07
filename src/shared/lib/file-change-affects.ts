// SPDX-License-Identifier: MIT
/** A coarse refresh, exact file event, or directory event containing this file. */
export function fileChangeAffects(
  changedPaths: Set<string> | null | undefined,
  filePath: string | null,
): boolean {
  if (!changedPaths) return true;
  if (!filePath) return false;
  for (const changed of changedPaths) {
    if (
      filePath === changed ||
      filePath.startsWith(changed.endsWith("/") ? changed : `${changed}/`)
    )
      return true;
  }
  return false;
}
