// SPDX-License-Identifier: MIT
import path from "path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { mockPanes } from "./scripts/vite-plugin-mock-panes";
import { worktreeMarker } from "./scripts/vite-plugin-worktree-marker";
import { webglAtlasGuard } from "./scripts/vite-plugin-webgl-atlas-guard";
import { excalidrawFonts } from "./scripts/vite-plugin-excalidraw-fonts";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [
    react(),
    tailwindcss(),
    webglAtlasGuard(),
    mockPanes(),
    worktreeMarker(__dirname),
    excalidrawFonts(__dirname),
  ],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
    // Put "worker" ahead of "browser" so packages used by the markdown
    // parse worker (`src/shared/lib/markdown-worker/`) resolve to their
    // Node-safe entry points. `decode-named-character-reference` is the
    // concrete trip wire: it ships `./index.dom.js` for browser (does
    // `document.createElement` at module top-level, crashes in a worker)
    // and `./index.js` for worker (uses a lookup table). The main-thread
    // bundle also picks the worker variant — harmless, just a lookup
    // table instead of a DOM hop.
    conditions: ["worker", "module", "browser", "development", "production"],
  },
  worker: {
    format: "es",
  },
  optimizeDeps: {
    // Scan the lazy worker up front so its imports cannot trigger a mid-session reload.
    // Explicit entries replace HTML inference, including its test/coverage exclusions.
    entries: [
      "**/*.html",
      "src/shared/lib/markdown-worker/parse.worker.ts",
      "!**/__tests__/**",
      "!**/coverage/**",
    ],
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      // 4. also ignore `.codesense/**` — codesense-managed config files
      //    (e.g. `.codesense/worktree.yaml`) live in user repos and in this
      //    repo itself during dev. Without this, editing worktree.yaml while
      //    `tauri dev` is running CodeSense on its own repo triggers
      //    a full Vite HMR reload, which looks like an app bug ("viewer
      //    flashes back to the splash screen") but is really just Vite.
      ignored: ["**/src-tauri/**", "**/.codesense/**"],
    },
  },
}));
