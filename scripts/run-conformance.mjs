// SPDX-License-Identifier: MIT
// With paths, run the per-dashboard checks; otherwise run the complete built-in suite.
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";

const paths = process.argv.slice(2).filter((arg) => arg !== "--");
const grepAt = paths.indexOf("--grep");
const grep = grepAt < 0 ? [] : paths.splice(grepAt, 2);
const slotAt = paths.indexOf("--slot");
const slotId = slotAt < 0 ? "right-pane" : paths.splice(slotAt, 2)[1];
if (!["dock", "right-pane", "file-nav"].includes(slotId) || (slotAt >= 0 && !paths.length)) {
  console.error("Usage: pnpm test:conformance [-- <pane.html>… [--slot dock|right-pane|file-nav]]");
  process.exit(1);
}
const result = spawnSync(
  "playwright",
  [
    "test",
    "-c",
    "playwright.conformance.config.ts",
    ...(paths.length ? ["conformance.spec.ts"] : []),
    ...grep,
  ],
  {
    stdio: "inherit",
    env: {
      ...process.env,
      ...(paths.length
        ? {
            CONFORMANCE_DASHBOARDS: JSON.stringify(
              paths.map((path) => ({ path: resolve(path), slotId })),
            ),
          }
        : {}),
    },
  },
);
process.exit(result.status ?? 1);
