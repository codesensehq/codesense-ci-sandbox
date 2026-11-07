// SPDX-License-Identifier: MIT
import "@testing-library/jest-dom/vitest";
import { afterEach, vi } from "vitest";

// Fixture Git must not inherit an agent's live tracing or daemon environment.
for (const key of Object.keys(process.env)) {
  if (key.startsWith("GIT_TRACE2") || key.startsWith("GIT_AI")) delete process.env[key];
}

// The terminal layout verbs are served by an in-memory host in every test file
// (see `src/test/terminal-layout-source-mock.ts`); `installTerminalLayoutHarness`
// wires its announcements into the viewer store.
vi.mock("@/features/viewer/lib/cli-workspace/transport/terminal-layout-source", async () => {
  const { terminalLayoutSourceMock } = await import("@/test/terminal-layout-source-mock");
  return terminalLayoutSourceMock;
});
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";

// SW-GT-3 · AC-8a: reset the status-taxonomy snapshot to the default seed after
// every test, so a suite that hydrates a CUSTOM taxonomy can't leak its states
// into later tests (the store is a module singleton seeded once at import).
afterEach(() => {
  useStatusTaxonomyStore.getState().resetForTest();
});

// The codebase's keyboard handling assumes macOS (the primary dev
// platform) — `CmdOrCtrl` → `metaKey`. happy-dom's default
// `navigator.platform` is platform-dependent and not reliably "Mac*",
// so existing tests that fire `{ metaKey: true }` would fail on CI.
// Stub to a Mac-ish value so the shortcut layer treats the test env
// as macOS. Individual tests that need non-Mac behavior can override
// per-case via `Object.defineProperty(navigator, "platform", ...)`.
// Files under `// @vitest-environment node` (the emulator worker's) have no DOM
// to stub, so the DOM shims below are skipped there.
const hasDom = typeof window !== "undefined";
if (hasDom) {
  Object.defineProperty(navigator, "platform", {
    value: "MacIntel",
    configurable: true,
    writable: true,
  });
}

// React Flow requires DOM APIs not fully available in happy-dom
// See: https://reactflow.dev/learn/advanced-use/testing
let reactFlowMockInit = false;
function mockReactFlow() {
  if (reactFlowMockInit) return;
  reactFlowMockInit = true;

  if (!global.DOMMatrixReadOnly) {
    (global as Record<string, unknown>).DOMMatrixReadOnly = class DOMMatrixReadOnly {
      m22: number;
      constructor(transform?: string) {
        const scale = transform?.match(/scale\(([0-9.]+)\)/)?.[1];
        this.m22 = scale !== undefined ? +scale : 1;
      }
    };
  }

  Object.defineProperties(global.HTMLElement.prototype, {
    offsetHeight: {
      get() {
        // Respect explicit inline `style.height` (including "0px") so tests
        // asserting collapsed-pane behavior aren't silently masked by the
        // fallbacks below.
        if (this.style.height) {
          const fromStyle = parseFloat(this.style.height);
          if (!Number.isNaN(fromStyle)) return fromStyle;
        }
        // happy-dom doesn't compute flexbox/grid layout, so flex-grown scroll
        // containers report 0. @tanstack/react-virtual reads offsetHeight at
        // mount to size its visible range — return a tall default for
        // overflow-y-auto / flex-1 containers so virtualized tests render
        // their full overscan window without each test mocking useVirtualizer.
        if (
          typeof this.className === "string" &&
          (this.className.includes("overflow-y-auto") || this.className.includes("flex-1"))
        ) {
          return 2000;
        }
        return 100;
      },
      configurable: true,
    },
    offsetWidth: {
      get() {
        return parseFloat(this.style.width) || 100;
      },
      configurable: true,
    },
  });

  if (!(global.SVGElement.prototype as unknown as Record<string, unknown>).getBBox) {
    (global.SVGElement.prototype as unknown as Record<string, unknown>).getBBox = () => ({
      x: 0,
      y: 0,
      width: 0,
      height: 0,
    });
  }
}

if (hasDom) mockReactFlow();

// SW-VE-7: the context nav (and the terminal tiles) subscribe to the webview
// drag-drop event on mount. happy-dom has no Tauri webview, so provide a safe
// no-op default globally — any test rendering those components would otherwise
// throw on `getCurrentWebview()`. Tests that assert drag-drop behavior override
// this per-file with their own event-dispatching mock.
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: () => Promise.resolve(() => {}) }),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: {
    error: vi.fn(),
    warn: vi.fn(),
    info: vi.fn(),
    debug: vi.fn(),
    trace: vi.fn(),
  },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

// Happy-dom lacks `Worker`. Tests exercise the sync `<ReactMarkdown>`
// render path (production uses the worker path when
// `VIEWER_PERF.parseInWorker` is true); flip the flag off globally so
// the markdown viewer doesn't try to instantiate a worker during tests.
vi.mock("@/shared/lib/viewer-perf-config", () => ({
  VIEWER_PERF: {
    maxBridgedFrames: 16,
    fileCacheMaxEntries: 25,
    keepAliveMaxViewers: 10,
    // Both flags off in tests — the viewer falls back to the synchronous
    // `<ReactMarkdown>` path (happy-dom has no Worker, and the block
    // path requires the worker to produce `Block[]`). `renderMode` is
    // unused when `parseInWorker` is false but kept present so the mock
    // shape matches production VIEWER_PERF exactly.
    parseInWorker: false,
    renderMode: "jsx" as const,
    // SW-FX-48: off in tests — the pipeline lays out synchronously on the main
    // thread (happy-dom has no Worker), so the canvas renders without an async
    // worker round-trip. The dedicated use-async-layout / run-layout suites
    // cover the worker path in isolation.
    layoutInWorker: false,
  },
}));
