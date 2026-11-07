// SPDX-License-Identifier: MIT
import { getActiveTeamKey } from "@/shared/lib/team-key";
import type { FrameAnchorRect } from "./host-menu-context";

export type NativeMenuRequest = {
  action?: "rename" | "delete" | "new-file" | "status" | "nav-filter";
  surface?: string;
  row: Record<string, unknown>;
  teamKey: string;
  contentRoot: string;
  anchorRect: FrameAnchorRect;
  owner: object;
  onClose: () => void;
};
type Opener = (request: NativeMenuRequest) => (() => void) | null;
const openers = new Map<string, Opener>();
export function registerNativeMenu(
  teamKey: string,
  surface: string,
  open: Opener,
  rowPath?: string,
) {
  const key = `${teamKey}/${surface}${rowPath ? `/${rowPath}` : ""}`;
  openers.set(key, open);
  return () => {
    if (openers.get(key) === open) openers.delete(key);
  };
}
export function openNativeMenu(request: NativeMenuRequest) {
  if (getActiveTeamKey() !== request.teamKey) return null;
  const team = request.row.team;
  if (
    team &&
    typeof team === "object" &&
    `${(team as { org?: string }).org}:${(team as { team?: string }).team}` !== request.teamKey
  )
    return null;
  const surface =
    request.surface ??
    (["workspace", "group"].includes(String(request.row.kind)) ? "workspace" : "nav");
  const path = typeof request.row.path === "string" ? `/${request.row.path}` : "";
  return (
    (path ? openers.get(`${request.teamKey}/${surface}${path}`) : undefined)?.(request) ??
    openers.get(`${request.teamKey}/${surface}`)?.(request) ??
    null
  );
}
