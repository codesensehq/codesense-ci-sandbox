// SPDX-License-Identifier: MIT
import fs from "fs";
import path from "path";
import type { Plugin } from "vite";

/** Where the app serves Excalidraw's fonts. `index.html` names it as `window.EXCALIDRAW_ASSET_PATH`. */
export const EXCALIDRAW_ASSET_BASE = "/excalidraw-assets/";

/** The package's Liberation Sans is 1.05 (GPL-2.0 with the font exception); the app ships 2.1.5 (OFL-1.1) under the same file name. */
const LIBERATION = "Liberation/";

/**
 * Excalidraw loads a canvas's fonts from `window.EXCALIDRAW_ASSET_PATH` and, failing that, from
 * esm.sh. This ships the package's own `dist/prod/fonts` at `EXCALIDRAW_ASSET_BASE`: copied into
 * the build, served by the dev server. Opening a canvas then asks no outside host. `Liberation/`
 * comes from `release/third-party/fonts/liberation-sans` instead of the package.
 */
export function excalidrawFonts(root: string): Plugin {
  const fonts = path.join(root, "node_modules/@excalidraw/excalidraw/dist/prod/fonts");
  const liberation = path.join(root, "release/third-party/fonts/liberation-sans");
  const prefix = `${EXCALIDRAW_ASSET_BASE}fonts/`;
  let outDir = "";
  return {
    name: "cs-excalidraw-fonts",
    configResolved(config) {
      outDir = path.resolve(config.root, config.build.outDir);
    },
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const url = req.url?.split("?")[0] ?? "";
        if (!url.startsWith(prefix)) return next();
        const rel = decodeURIComponent(url.slice(prefix.length));
        const [dir, name] = rel.startsWith(LIBERATION)
          ? [liberation, rel.slice(LIBERATION.length)]
          : [fonts, rel];
        const file = path.join(dir, name);
        if (!file.startsWith(dir + path.sep)) return next();
        if (!fs.statSync(file, { throwIfNoEntry: false })?.isFile()) return next();
        res.setHeader("content-type", "font/woff2");
        fs.createReadStream(file).pipe(res);
      });
    },
    writeBundle() {
      const out = path.join(outDir, prefix);
      fs.cpSync(fonts, out, { recursive: true });
      fs.rmSync(path.join(out, LIBERATION), { recursive: true, force: true });
      fs.cpSync(liberation, path.join(out, LIBERATION), { recursive: true });
    },
  };
}
