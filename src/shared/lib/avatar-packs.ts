// SPDX-License-Identifier: MIT
import type { AvatarFaceView, AvatarPackView } from "@/tauri/commands";

import funnyManifest from "@/assets/avatar-packs/builtin-funny/pack.json";
import chessManifest from "@/assets/avatar-packs/builtin-chess/pack.json";
import crestManifest from "@/assets/avatar-packs/builtin-crest/pack.json";

/**
 * Avatar packs: folders of faces an agent can wear, named everywhere as `pack:id`. Three ship
 * with the app (bundled here); the rest live in the team's content or the user's scratch
 * space and come from the backend as `data:` URLs. Display only.
 */

interface BundledManifest {
  name: string;
  faces: { id: string; file: string; label?: string }[];
}

const BUNDLED_FILES = import.meta.glob<string>("/src/assets/avatar-packs/*/*.svg", {
  eager: true,
  query: "?url",
  import: "default",
});

function bundledPack(id: string, manifest: BundledManifest): AvatarPackView {
  return {
    id,
    name: manifest.name,
    scope: "builtin",
    faces: manifest.faces.flatMap((face) => {
      const src = BUNDLED_FILES[`/src/assets/avatar-packs/${id}/${face.file}`];
      return src ? [{ id: face.id, ref: `${id}:${face.id}`, label: face.label ?? null, src }] : [];
    }),
  };
}

/** The pack the app falls back to when nothing else names a face. */
export const BUILTIN_FUNNY_PACK = "builtin-funny";

/** The packs the app ships, in the order a face menu lists them. */
export const BUILTIN_AVATAR_PACKS: readonly AvatarPackView[] = [
  bundledPack("builtin-chess", chessManifest),
  bundledPack(BUILTIN_FUNNY_PACK, funnyManifest),
  bundledPack("builtin-crest", crestManifest),
];

const REF = /^([a-z0-9][a-z0-9-]{0,31}):([a-z0-9][a-z0-9-]{0,30})$/;

/** `[pack, id]` when `value` is a `pack:id` reference. */
export function parseFaceRef(value: string | null | undefined): [string, string] | null {
  const match = REF.exec(value?.trim() ?? "");
  return match ? [match[1], match[2]] : null;
}

/** Every face of `packs` by its ref. A pack id listed twice keeps its first listing. */
export function indexFaces(packs: readonly AvatarPackView[]): Map<string, AvatarFaceView> {
  const index = new Map<string, AvatarFaceView>();
  const seen = new Set<string>();
  for (const pack of packs) {
    if (seen.has(pack.id)) continue;
    seen.add(pack.id);
    for (const face of pack.faces) if (!index.has(face.ref)) index.set(face.ref, face);
  }
  return index;
}

/**
 * The image for a face ref, or null when it is not a ref or its pack or face is not on this
 * machine (a teammate's personal pack, a pack not synced yet): the caller then falls back to
 * its next choice.
 */
export function resolveFaceRef(
  index: ReadonlyMap<string, AvatarFaceView>,
  ref: string | null | undefined,
): string | null {
  const parsed = parseFaceRef(ref);
  if (!parsed) return null;
  return index.get(`${parsed[0]}:${parsed[1]}`)?.src ?? null;
}

/**
 * What the face resolver reads: images by ref, each agent's own default face, and the pool
 * an agent nobody gave a face is dealt from.
 */
export interface FaceSource {
  /** The image for a `pack:id` ref, or null when it is not available here. */
  resolve: (ref: string | null | undefined) => string | null;
  /** An agent definition's own default face (a ref, a colour or an emoji), by agent key. */
  agentAvatar: (agentKey: string) => string | null;
  /** The faces dealt to agents with none: the team's default pack, else the built-in funny one. */
  pool: readonly AvatarFaceView[];
}

/** A face source over `packs`, dealing from `defaultPack` when it has faces. */
export function faceSource(
  packs: readonly AvatarPackView[],
  opts: { defaultPack?: string | null; agentAvatars?: Readonly<Record<string, string>> } = {},
): FaceSource {
  const index = indexFaces(packs);
  const chosen = packs.find((p) => p.id === opts.defaultPack)?.faces ?? [];
  const funny = packs.find((p) => p.id === BUILTIN_FUNNY_PACK)?.faces ?? [];
  const avatars = opts.agentAvatars ?? {};
  return {
    resolve: (ref) => resolveFaceRef(index, ref),
    agentAvatar: (key) => avatars[key] ?? null,
    pool: chosen.length > 0 ? chosen : funny,
  };
}

/** The built-in packs alone: what faces resolve against before the team's packs are read. */
export const BUILTIN_FACE_SOURCE: FaceSource = faceSource(BUILTIN_AVATAR_PACKS);
