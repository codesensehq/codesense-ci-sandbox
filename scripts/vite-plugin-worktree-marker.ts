// SPDX-License-Identifier: MIT
import fs from "fs";
import type { Plugin } from "vite";

export const WORKTREE_MARKER_PATH = "/__cs-worktree";

/**
 * Dev-server-only marker: `GET /__cs-worktree` answers `{ root, pid }` so the Playwright
 * guard (`playwright-worktree-guard.ts`) can tell whose checkout a reused server runs.
 * `apply: "serve"` keeps it out of every `vite build` (app, beta, published bundles).
 */
export function worktreeMarker(root: string): Plugin {
  const realRoot = fs.realpathSync(root);
  return {
    name: "cs-worktree-marker",
    apply: "serve",
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        if (req.url?.split("?")[0] !== WORKTREE_MARKER_PATH) return next();
        res.setHeader("content-type", "application/json");
        res.setHeader("cache-control", "no-store");
        res.end(JSON.stringify({ root: realRoot, pid: process.pid }));
      });
    },
  };
}
