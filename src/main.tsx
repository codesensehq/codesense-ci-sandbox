// SPDX-License-Identifier: MIT
import { AppThemeProvider } from "@/themes/theme-provider";
import { HostMenuLayer } from "@/features/shell/host-menu-layer";
import React from "react";
import ReactDOM from "react-dom/client";
import { ThemeProvider } from "next-themes";
import type { useConfigStore } from "@/shared/stores/config-store";
import type { useOrgStore } from "@/shared/stores/org-store";
import type { useUiStore } from "@/shared/stores/ui-store";
import type { useViewerStore } from "@/shared/stores/viewer-store";
import { RootCrashFallback } from "@/features/shell/components/root-crash-fallback";
import { ErrorBoundary } from "@/shared/components/error-boundary";
import { initCodesenseHome } from "@/shared/lib/codesense-home";
import { initLogging } from "@/shared/lib/logger";
import App from "./app/App";
import { VerbConfirmationProvider } from "@/features/dashboard/components/verb-confirmation-provider";
import { installGlobalErrorHandlers } from "./app/install-global-error-handlers";
import { startRendererHeartbeat } from "./app/renderer-heartbeat";
import { installMockBackend } from "./app/mock/install";
import "./index.css";

if (import.meta.env.MODE === "mock") {
  // Frontend-only dev mode: no Rust. Plant the mock `window.__TAURI_INTERNALS__`
  // synchronously and FIRST — before React renders — so the api's synchronous
  // metadata reads (`getCurrentWindow()`) and the first `invoke` find the shim
  // instead of throwing `window.__TAURI_INTERNALS__ is undefined`. `MODE` is
  // statically replaced, so this branch is dead in prod/webdriver builds and the
  // whole mock module graph tree-shakes out (proven by verify-mock-absence.ts).
  installMockBackend();
}

if (import.meta.env.MODE === "webdriver") {
  // Arm the harness-only listener counter as the first thing in webdriver mode
  // (QH-5 AC-2 INSTRUMENTED signal). This is a code-split dynamic import, so it
  // resolves on an early microtask — a handful of listeners from the initial
  // synchronous render may pre-date it, which is immaterial: the soak's oracle
  // measures per-iteration GROWTH over a window that starts long after boot, not
  // the absolute baseline. Code-split like the plugin below → tree-shaken from
  // prod and proven absent by scripts/verify-webdriver-absence.ts.
  void import("./app/e2e-listener-counter").then((m) => m.installListenerCounter());
  void import("@wdio/tauri-plugin");
}

// DEV-only console introspection shim — exposes the Zustand stores on
// `window.__stores` so devtools sessions can snapshot/inspect state
// (`window.__stores.useOrgStore.getState()` etc.). Guarded by
// `import.meta.env.DEV` so Vite tree-shakes the whole block (and the
// dynamic imports of the store modules) out of production builds —
// zero exposure to end users. The named-export checks below catch
// silent shape drift if any store is renamed in a future refactor.
declare global {
  interface Window {
    __stores?: {
      useOrgStore: typeof useOrgStore;
      useViewerStore: typeof useViewerStore;
      useUiStore: typeof useUiStore;
      useConfigStore: typeof useConfigStore;
    };
  }
}

if (import.meta.env.DEV) {
  void Promise.all([
    import("@/shared/stores/org-store"),
    import("@/shared/stores/viewer-store"),
    import("@/shared/stores/ui-store"),
    import("@/shared/stores/config-store"),
  ])
    .then(([org, viewer, ui, config]) => {
      const missing: string[] = [];
      if (!org.useOrgStore) missing.push("useOrgStore");
      if (!viewer.useViewerStore) missing.push("useViewerStore");
      if (!ui.useUiStore) missing.push("useUiStore");
      if (!config.useConfigStore) missing.push("useConfigStore");
      if (missing.length > 0) {
        console.warn(
          `[dev-shim] store named-export(s) missing — shim not installed: ${missing.join(", ")}`,
        );
        return;
      }
      window.__stores = {
        useOrgStore: org.useOrgStore,
        useViewerStore: viewer.useViewerStore,
        useUiStore: ui.useUiStore,
        useConfigStore: config.useConfigStore,
      };
      console.info(
        "[dev-shim] stores exposed on window.__stores — e.g. window.__stores.useOrgStore.getState()",
      );
    })
    .catch((err: unknown) => {
      console.warn("[dev-shim] store import failed — shim not installed:", err);
    });
}

// SF-11: establish file logging as early as possible, then register the global
// error/rejection handlers before first render. `initLogging()` loads
// tauri-plugin-log + `attachConsole` (idempotent — App's boot effect awaits the
// same init), so the earliest crash record reaches the log FILE, not just the
// devtools console. Fire-and-forget: the handlers themselves don't await it.
void initLogging();
installGlobalErrorHandlers();

// SW-FX-64 Layer 1: start the renderer-liveness heartbeat at module scope,
// OUTSIDE the React tree, so a wedged/crashed subtree can't starve the ack. If
// the renderer stops acking (OOM / content-process death), the Rust watchdog
// reloads the webview instead of leaving the window blank until restart.
startRendererHeartbeat();

// SR-11: dev-only User Timing buffer reclaim. The vite DEV bundle's React build
// emits a `performance.measure()` per render into WebKit's unbounded User Timing
// buffer (6.4M entries ≈ 7 GB in the incident renderer), which beachballs dev.
// DEV-gated dynamic import → code-split OUT of the production bundle (users carry
// zero cost); mounted at module scope alongside the heartbeat so the sweep can't
// be starved by the very subtree wedge it exists to relieve.
if (import.meta.env.DEV) {
  void import("./app/dev-user-timing-reclaim").then((m) => m.startDevUserTimingReclaim());
}

// Resolve ~/.codesense BEFORE the first render: `getCodesenseHome()` is read
// synchronously across the tree and must never be unresolved. A failed read
// still renders (the home is then null, which callers already handle).
void initCodesenseHome().finally(
  isWorktreeEditorHarness() ? renderWorktreeEditorHarness : renderApp,
);

function renderApp(): void {
  if (
    import.meta.env.MODE === "mock" &&
    new URLSearchParams(window.location.search).get("harness") === "dock-sources"
  ) {
    void import("./app/mock/dock-source-harness").then(({ DockSourceHarness }) => {
      ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
        <ThemeProvider attribute="class" defaultTheme="dark">
          <DockSourceHarness />
        </ThemeProvider>,
      );
    });
    return;
  }
  if (
    import.meta.env.MODE === "mock" &&
    new URLSearchParams(window.location.search).get("harness") === "slots"
  ) {
    void import("./app/mock/slot-harness")
      .then(async ({ SlotHarness }) => {
        const { readPrefsState, usePrefsStore } =
          await import("@/features/dashboard/lib/prefs-client");
        await readPrefsState();
        if (!usePrefsStore.getState().ownedReady)
          throw new Error("Slot harness requires an active local profile");
        ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
          <ThemeProvider attribute="class" defaultTheme="dark">
            <SlotHarness />
          </ThemeProvider>,
        );
      })
      .catch(console.error);
    return;
  }
  if (
    import.meta.env.MODE === "mock" &&
    new URLSearchParams(window.location.search).get("harness") === "live-hub"
  ) {
    void import("./app/mock/live-hub-harness").then(({ LiveHubHarness }) => {
      ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
        <ThemeProvider attribute="class" defaultTheme="dark">
          <LiveHubHarness />
        </ThemeProvider>,
      );
    });
    return;
  }

  // `dev:mock` only: `?harness=source-diff` mounts the source-diff tool alone on
  // its fixture world. Dead (and tree-shaken) outside mock builds.
  if (
    import.meta.env.MODE === "mock" &&
    new URLSearchParams(window.location.search).get("harness") === "source-diff"
  ) {
    void import("./app/mock/source-diff-harness").then(({ SourceDiffHarness }) => {
      ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
        <ThemeProvider attribute="class" defaultTheme="dark" enableSystem>
          <SourceDiffHarness />
        </ThemeProvider>,
      );
    });
    return;
  }
  // `dev:mock` only: `?harness=progress` mounts every SW-XS-10 progress state
  // on one page (setup line, tray pills, operation chip).
  if (
    import.meta.env.MODE === "mock" &&
    new URLSearchParams(window.location.search).get("harness") === "progress"
  ) {
    void import("./app/mock/progress-harness").then(({ ProgressHarness }) => {
      ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
        <ThemeProvider attribute="class" defaultTheme="dark" enableSystem>
          <ProgressHarness />
        </ThemeProvider>,
      );
    });
    return;
  }
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <ThemeProvider attribute="class" defaultTheme="system" enableSystem>
        <AppThemeProvider />
        {/* SF-11: root error backstop — a render throw that escapes every inner
          boundary degrades to RootCrashFallback instead of blanking the window. */}
        <ErrorBoundary scope="root" fallback={<RootCrashFallback />}>
          <VerbConfirmationProvider>
            <HostMenuLayer>
              <App />
            </HostMenuLayer>
          </VerbConfirmationProvider>
        </ErrorBoundary>
      </ThemeProvider>
    </React.StrictMode>,
  );
}

/** `dev:mock` only: `?harness=worktree-editor` mounts the worktree code editor
 *  alone on its fixture world. Dead (and tree-shaken) outside mock builds. */
function isWorktreeEditorHarness(): boolean {
  return (
    import.meta.env.MODE === "mock" &&
    new URLSearchParams(window.location.search).get("harness") === "worktree-editor"
  );
}

function renderWorktreeEditorHarness(): void {
  void import("./app/mock/worktree-editor-harness").then(({ WorktreeEditorHarness }) => {
    ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
      <ThemeProvider attribute="class" defaultTheme="dark" enableSystem>
        <WorktreeEditorHarness />
      </ThemeProvider>,
    );
  });
}
