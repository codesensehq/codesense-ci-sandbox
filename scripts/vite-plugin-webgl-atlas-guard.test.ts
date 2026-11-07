// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { checkBundleForAtlasSharing } from "./vite-plugin-webgl-atlas-guard";

// The sharing loop as Vite/esbuild actually minified it into a shipped bundle.
const MINIFIED_SHARING_LOOP =
  "for(let h=0;h<Uc.length;h++){let p=Uc[h];if(Uce(p.config,u))return p.ownedBy.push(e),p.atlas}" +
  "let d=e._core,f={atlas:new fm(document,u,d.unicodeService),config:u,ownedBy:[e]};";
const PATCHED_ACQUIRE =
  "let d=e._core,f={atlas:new fm(document,u,d.unicodeService),config:u,ownedBy:[e]};return Uc.push(f),f.atlas";

describe("checkBundleForAtlasSharing", () => {
  it("fails a bundle that still contains the cross-terminal sharing loop", () => {
    const result = checkBundleForAtlasSharing({ "assets/index-x.js": MINIFIED_SHARING_LOOP });
    expect(result.ok).toBe(false);
    expect(result.reason).toContain("assets/index-x.js");
  });

  it("passes a bundle whose atlas cache has no sharing loop", () => {
    expect(checkBundleForAtlasSharing({ "assets/index-x.js": PATCHED_ACQUIRE }).ok).toBe(true);
  });

  it("fails closed when the atlas cache cannot be found at all", () => {
    const result = checkBundleForAtlasSharing({ "assets/index-x.js": "console.log(1)" });
    expect(result.ok).toBe(false);
    expect(result.reason).toContain("cannot verify");
  });

  it("passes the installed (patched) ESM addon entry that Vite bundles", () => {
    const mjs = readFileSync(
      join(process.cwd(), "node_modules/@xterm/addon-webgl/lib/addon-webgl.mjs"),
      "utf8",
    );
    expect(checkBundleForAtlasSharing({ "addon-webgl.mjs": mjs }).ok).toBe(true);
  });
});
