// SPDX-License-Identifier: MIT
import type { Plugin } from "vite";

/**
 * Build guard: fail any Vite build whose output still contains `@xterm/addon-webgl`'s
 * cross-terminal atlas-sharing loop.
 *
 * Upstream `acquireTextureAtlas` joins a new terminal to an existing same-config atlas
 * via `entry.ownedBy.push(terminal)`; our pnpm patch deletes that loop so every terminal
 * mints its own atlas (a shared atlas lets one pane's clear corrupt its siblings). The
 * check reads the BUNDLED code, so it holds whichever package entry the resolver picked.
 *
 * Keyed on property names, which minifiers keep (comments and local names they don't):
 * - `.ownedBy.push(` exists only in the sharing loop → its presence fails the build;
 * - `ownedBy:[` is the cache-entry literal that remains after the patch → at least one
 *   chunk must contain it, so an upstream rename can't turn the guard into a silent pass.
 */
export const SHARING_LOOP = /\.ownedBy\.push\(/;
export const ATLAS_CACHE_ENTRY = /ownedBy:\[/;

export interface AtlasGuardResult {
  ok: boolean;
  reason?: string;
}

export function checkBundleForAtlasSharing(chunks: Record<string, string>): AtlasGuardResult {
  const sharing = Object.keys(chunks).filter((name) => SHARING_LOOP.test(chunks[name]));
  if (sharing.length > 0) {
    return {
      ok: false,
      reason:
        `@xterm/addon-webgl atlas-sharing loop found in ${sharing.join(", ")}: the un-share ` +
        "patch is not in the bundled entry (see patches/README.md).",
    };
  }
  if (!Object.values(chunks).some((code) => ATLAS_CACHE_ENTRY.test(code))) {
    return {
      ok: false,
      reason:
        "@xterm/addon-webgl atlas cache not found in the bundle: cannot verify the un-share " +
        "patch (was the addon upgraded or renamed?). Update this guard alongside the patch.",
    };
  }
  return { ok: true };
}

export function webglAtlasGuard(): Plugin {
  return {
    name: "codesense:webgl-atlas-unshared",
    apply: "build",
    enforce: "post",
    // generateBundle (so a failing bundle is never written) with `enforce: "post"` so
    // Vite's own html plugin has already emitted index.html, which identifies the app
    // build below.
    generateBundle(_options, bundle) {
      // Worker sub-builds run this hook too but never include the addon; only the app
      // build (the one emitting index.html) is judged.
      const outputs = Object.values(bundle);
      if (!outputs.some((o) => o.type === "asset" && o.fileName.endsWith(".html"))) return;
      const chunks: Record<string, string> = {};
      for (const output of outputs) {
        if (output.type === "chunk") chunks[output.fileName] = output.code;
      }
      const result = checkBundleForAtlasSharing(chunks);
      if (!result.ok) this.error(result.reason ?? "webgl atlas guard failed");
    },
  };
}
