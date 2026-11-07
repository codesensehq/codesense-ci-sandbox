// SPDX-License-Identifier: MIT
/**
 * `pnpm test:conformance -- <dashboard.html>…` runs every check against each dashboard in
 * real WebKit; with no paths it runs the built-in set. Each failure names the check and why.
 */
import { basename, resolve } from "node:path";

import { test, type PaneFixtures } from "../pane-harness/harness";
import { AGENT_LIST_PANE } from "../pane-harness/agent-list-fixtures";
import { CHECKS, runCheck } from "./checks";
import { builtInPaneFiles } from "../../scripts/lib/builtin-panes.mjs";

const builtIns: string[] = builtInPaneFiles().filter((path: string) => path.endsWith(".html"));
const FILES_TREE_PANE = "src/features/shell/slots/builtin/files-tree.html";
type Dashboard = string | { path: string; slotId?: PaneFixtures["slotId"] };
const dashboards: Dashboard[] = process.env.CONFORMANCE_DASHBOARDS
  ? JSON.parse(process.env.CONFORMANCE_DASHBOARDS)
  : [
      ...builtIns.flatMap<Dashboard>((path) =>
        path === FILES_TREE_PANE
          ? [
              { path, slotId: "file-nav" as const },
              { path, slotId: "right-pane" as const },
            ]
          : path === AGENT_LIST_PANE
            ? [{ path, slotId: "dock" as const }]
            : [path],
      ),
      "codesense-docs/getting-started/content/examples/dashboards/live-list-with-command.html",
    ];

// Policy: external panes may opt into strict theming; built-ins always fail.
const NON_BUILT_IN_THEME_POLICY: "warn" | "fail" =
  process.env.CONFORMANCE_THEME_POLICY === "fail" ? "fail" : "warn";
const builtInPaths = new Set(builtIns.map((path) => resolve(path)));
const isBuiltIn = (path: string) => builtInPaths.has(resolve(path));

for (const entry of dashboards) {
  const { path, slotId = "right-pane" } = typeof entry === "string" ? { path: entry } : entry;
  test.describe(`${basename(path)} (${slotId})`, () => {
    for (const check of CHECKS)
      test(check.title, async ({ mountPane }) => {
        if (check.id === "theme") test.setTimeout(90_000);
        try {
          await runCheck(mountPane, resolve(path), check, slotId);
        } catch (error) {
          if (check.id === "theme" && !isBuiltIn(path) && NON_BUILT_IN_THEME_POLICY === "warn") {
            console.warn(
              `${path}: doesn't follow theme: ${error instanceof Error ? error.message : String(error)}`,
            );
            return;
          }
          throw error;
        }
      });
  });
}
