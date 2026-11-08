// SPDX-License-Identifier: MIT
import { quarantineId } from "../../scripts/lib/quarantine.mjs";
import { test as base } from "@playwright/test";
import { quarantineReason } from "./quarantine";

export * from "@playwright/test";

export const test = base.extend<{ quarantine: void }>({
  quarantine: [
    async ({ browserName: _browserName }, use, info) => {
      const reason = quarantineReason(
        quarantineId("playwright", info.file, info.titlePath.slice(1)),
      );
      if (reason) {
        console.warn(reason);
        base.fixme(true, reason);
      }
      await use();
    },
    { auto: true },
  ],
});
