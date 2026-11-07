// SPDX-License-Identifier: MIT
import path from "path";
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "happy-dom",
    globals: true,
    setupFiles: ["./src/test-setup.ts"],
    // SF-5: unit tests never have a dev server, so any resource a
    // component injects into the DOM makes happy-dom fire a real network
    // request against the default origin (http://localhost:3000) or a
    // mocked server port. Two surfaces do this today:
    //   - markdown-viewer's `<link rel="stylesheet" href="/hljs/*.css">`
    //     theme loader (useHighlightTheme) → ECONNREFUSED to :3000.
    //   - html-viewer's `<iframe src="http://localhost:<port>/viewer/…">`.
    // happy-dom catches these internally and logs them — they are console
    // *noise*, not unhandled rejections (the real exit-1 culprit was a
    // missing `closeTeam` store mock; see App.coverage.test.tsx). But the
    // noise mimics real failures and is exactly what masked that root
    // cause. Disabling resource loading at the environment boundary stops
    // the network I/O for the whole class — every injected stylesheet,
    // script, or iframe — with no production-code branch and no weakened
    // assertion: elements still mount and expose href/src for tests to
    // assert against. `handleDisabledFileLoadingAsSuccess` makes link/
    // script dispatch `load` (not `error`), so the CSS/JS paths go silent.
    // happy-dom 21 leaves CSS and iframe page loading off by default, so
    // only JavaScript loading needs disabling here.
    environmentOptions: {
      happyDOM: {
        settings: {
          disableJavaScriptFileLoading: true,
          handleDisabledFileLoadingAsSuccess: true,
        },
      },
    },
    // Note: the iframe path has no "treat-as-success" escape hatch like
    // CSS/JS, so happy-dom still logs a benign `NotSupportedError`
    // ("Iframe page loading is disabled") for every viewer iframe a test
    // renders. It fires during async teardown — outside vitest's
    // test-scoped console capture — so neither `onConsoleLog` nor a
    // setupFile console patch can reach it. Left as-is: it is harmless
    // (exit stays 0), no longer hits the network, and self-identifies as
    // intentional rather than mimicking a real failure.
    // e2e/e2e-real live under Playwright, not the unit runner (e2e-real helpers
    // import deps like `pngjs` that aren't in the unit context). `.claude/**` keeps
    // stale/parallel agent worktrees — full source copies that may reference
    // since-deleted modules — from being globbed into the run.
    exclude: [
      "tests/e2e/**",
      "tests/frame-security/**",
      // pane-harness/: Playwright specs under their own config.
      "tests/pane-harness/**",
      "tests/e2e-real/**",
      "tests/e2e-mock/**",
      // dogfood/ and e2e-real/: maintainer-only Playwright and WebdriverIO walks, when present.
      "tests/dogfood/**",
      // conformance/: Playwright specs under playwright.conformance.config.ts.
      "tests/conformance/**",
      // Playwright specs use *.spec.*; Vitest tests use *.test.*.
      "**/*.spec.*",
      // codesense.ai site: its own toolchain and node_modules (Playwright only).
      "site/**",
      // node:test files, run by `pnpm test:node-scripts`.
      "src-tauri/resources/opencode/*.test.mjs",
      "node_modules/**",
      "**/.claude/**",
    ],
    coverage: {
      provider: "v8",
      reporter: ["text", "text-summary", "json-summary", "html"],
      reportsDirectory: "./coverage",
      include: [
        "src/shared/stores/**",
        "src/tauri/**",
        "src/shared/lib/**",
        "src/lib/**",
        "src/features/**/components/**",
        "src/features/**/views/**",
        "src/app/**",
      ],
      exclude: [
        "**/*.test.*",
        "**/*.spec.*",
        "src/components/ui/**",
        "src/test-setup.ts",
        "src/vite-env.d.ts",
        "src/main.tsx",
      ],
      // Ratcheted 2026-04-04 — stores at 95/76/93/96 actual
      thresholds: {
        "src/shared/stores/**": {
          statements: 90,
          branches: 70,
          functions: 80,
          lines: 90,
        },
      },
    },
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
});
