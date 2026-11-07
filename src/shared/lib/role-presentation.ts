// SPDX-License-Identifier: MIT
import type { RoleConfig } from "@/tauri/commands";
import { BUILTIN_AVATAR_PACKS, faceSource, type FaceSource } from "./avatar-packs";

const AUTO_PIECES = ["king", "rook", "bishop", "knight"] as const;
export function defaultRoleIcon(id: string): string {
  let hash = 2166136261;
  for (const char of id) hash = Math.imul(hash ^ char.codePointAt(0)!, 16777619) >>> 0;
  return `builtin-chess:${AUTO_PIECES[hash % AUTO_PIECES.length]}`;
}
const BUILTIN = faceSource(BUILTIN_AVATAR_PACKS);
export function rolePresentation(
  id: string,
  configs: readonly RoleConfig[],
  faces: Pick<FaceSource, "resolve"> = BUILTIN,
) {
  const config = configs.find((r) => r.id === id);
  const fallback = defaultRoleIcon(id);
  const ref = config?.icon && faces.resolve(config.icon) ? config.icon : fallback;
  const name = config?.name || id;
  return {
    id,
    name,
    ref,
    src: faces.resolve(ref) ?? BUILTIN.resolve(fallback)!,
    configured: !!config,
    title: `Queue role: ${name} (role:${id})`,
  };
}
