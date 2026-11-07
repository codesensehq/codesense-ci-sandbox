/* eslint-disable react-hooks/rules-of-hooks -- Playwright fixture, not a React component; `use` is Playwright's fixture API, not React's Hook */
import { test as base, expect } from "@playwright/test";
import { getTauriMockInitScript, type TauriMockCall } from "./tauri-ipc-mock";

export { isTerminalProcessCommand } from "./tauri-ipc-mock";
export type { TauriMockCall } from "./tauri-ipc-mock";

const tauriErrorsByPage = new WeakMap<import("@playwright/test").Page, string[]>();
const tauriPageErrorsByPage = new WeakMap<import("@playwright/test").Page, string[]>();

/**
 * Extended Playwright fixtures with Tauri IPC mocking.
 *
 * The `tauriApp` fixture injects mock IPC handlers via addInitScript
 * before navigation, then waits for the app to fully initialize past
 * the splash screen and auth gate.
 *
 * Usage:
 *   import { test, expect } from "../support/fixtures/tauri";
 *   test("my test", async ({ tauriApp }) => { ... });
 */

export interface TauriFixtures {
  /**
   * A page that has been pre-configured with Tauri IPC mocks and
   * navigated past the auth gate. Ready for interacting with
   * the repo picker, viewer, etc.
   */
  tauriApp: import("@playwright/test").Page;

  /**
   * Optional command overrides. Set via test.use({ tauriOverrides: {...} }).
   * Keys are Tauri command names, values are the mock return values.
   */
  tauriOverrides: Record<string, unknown>;

  /** Reads the redacted per-page IPC invocation ledger. */
  tauriCalls: () => Promise<TauriMockCall[]>;

  /** Reads browser errors observed by the assembled-app fixture. */
  tauriErrors: () => string[];

  /** Reads uncaught page errors without including React console diagnostics. */
  tauriPageErrors: () => string[];
}

export const test = base.extend<TauriFixtures>({
  tauriOverrides: [{}, { option: true }],

  tauriApp: async ({ page, tauriOverrides }, use) => {
    // Suppress console errors from any missed commands during init
    const consoleErrors: string[] = [];
    const pageErrors: string[] = [];
    page.on("pageerror", (err) => {
      consoleErrors.push(err.message);
      pageErrors.push(err.message);
    });
    page.on("console", (message) => {
      if (message.type() === "error") consoleErrors.push(message.text());
    });

    // Inject mock IPC before any app JS loads
    const initScript = getTauriMockInitScript(tauriOverrides);
    await page.addInitScript(initScript);

    // Navigate to the app
    await page.goto("/");

    // Wait for the app to get past the splash screen.
    // T.6: RepoPickerView retired — with the mock's authenticated state,
    // populated org, but no active team selection (the mock doesn't write
    // `activeTeamName`), the app lands on `TeamWorkspaceEmptyState` which
    // shows one of three branches depending on org + team membership state.
    // Any of those branches is a valid "past splash + auth gate" signal.
    // We also accept viewer content (README.md) when a follow-on test has
    // wired up enough state to mount a workspace.
    try {
      await page.waitForFunction(
        () => {
          const root = document.querySelector("#root");
          if (!root) return false;
          const text = root.textContent || "";
          return (
            // T.6 empty-state heading variants (TeamWorkspaceEmptyState).
            text.includes("Pick an org or create a new one") ||
            text.includes("Pick a team in ") ||
            text.includes("Add a source repo to ") ||
            // SW-OC-4 team-workspace landing empty-state (auto-local floor): the
            // actionable "connect your code" CTA the newcomer lands on.
            text.includes("Connect your code") ||
            text.includes("Add a source repo") ||
            // Viewer mode — file tree or markdown content visible.
            text.includes("README.md") ||
            // Source-free team landing: the files tree beside the open-a-file prompt.
            text.includes("Open a file from the sidebar") ||
            // No context repo at all: the setup (connect or create, no account).
            text.includes("Context repos") ||
            // SW-OC-16 first launch: the bundled CodeSense docs are active.
            document.querySelector('[data-context-repo="codesense-docs"]') !== null
          );
        },
        undefined,
        { timeout: 30_000 },
      );
    } catch (error) {
      if (consoleErrors.length > 0) {
        throw new Error(`App initialization failed: ${consoleErrors.join(" | ")}`, {
          cause: error,
        });
      }
      throw error;
    }

    tauriErrorsByPage.set(page, consoleErrors);
    tauriPageErrorsByPage.set(page, pageErrors);
    await use(page);
  },

  tauriCalls: async ({ tauriApp }, use) => {
    await use(() =>
      tauriApp.evaluate(
        () =>
          (
            window as unknown as {
              __TAURI_MOCK__?: { calls?: TauriMockCall[] };
            }
          ).__TAURI_MOCK__?.calls ?? [],
      ),
    );
  },

  tauriErrors: async ({ tauriApp }, use) => {
    await use(() => [...(tauriErrorsByPage.get(tauriApp) ?? [])]);
  },

  tauriPageErrors: async ({ tauriApp }, use) => {
    await use(() => [...(tauriPageErrorsByPage.get(tauriApp) ?? [])]);
  },
});

export { expect };
