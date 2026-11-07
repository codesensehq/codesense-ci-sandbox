// SPDX-License-Identifier: MIT
import { itemFilePath, type FlatNavItem } from "@/features/dashboard/lib/flatten-nav-tree";
import type { NavContextMenuState } from "./types";

export function dashboardNavTarget(
  row: Record<string, unknown>,
  contentRoot: string,
  items: FlatNavItem[],
): Omit<NavContextMenuState, "x" | "y"> | null {
  if (
    !["story", "epic", "file"].includes(String(row.kind)) ||
    typeof row.path !== "string" ||
    !row.path ||
    row.path
      .replace(/^\//, "")
      .split("/")
      .some((p) => p === ".." || p === "." || !p)
  )
    return null;
  const root = contentRoot.replace(/\/+$/, "");
  const path = row.path.startsWith("/") ? row.path : `${root}/${row.path}`;
  for (const item of items) {
    if (
      row.kind === "epic" &&
      item.type === "folder-header" &&
      (item.node.absoluteDirPath === path || item.node.mainDoc?.filePath === path)
    )
      return {
        path: item.node.absoluteDirPath,
        epicDirPath: item.node.dirPath,
        epicFilePath: item.node.mainDoc?.filePath,
        isEpic: item.node.mainDoc !== null,
      };
    if (itemFilePath(item) === path && (row.kind === "file" || item.type === "story"))
      return { path };
  }
  return row.kind === "file" && path.startsWith(`${root}/`) ? { path } : null;
}
