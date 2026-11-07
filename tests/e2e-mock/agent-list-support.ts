// SPDX-License-Identifier: MIT
import type { Page } from "@playwright/test";
import { readFileSync } from "node:fs";

export async function routeAgentListKit(page: Page) {
  await page.route("**/kit/v1/**", async (route) => {
    const name = new URL(route.request().url()).pathname.slice("/kit/v1/".length);
    if (!["kit.js", "virtual-list.js", "tokens.css", "kit.css", "fonts/geist.woff2"].includes(name))
      return route.continue();
    await route.fulfill({
      contentType: name.endsWith("woff2")
        ? "font/woff2"
        : name.endsWith("css")
          ? "text/css"
          : "text/javascript",
      body: readFileSync(`src-tauri/src/http/routes/dashboard_kit/${name}`),
    });
  });
}

export async function installAgentListPane(page: Page, source: string, paneId = "scale") {
  const kit =
    '<link rel="stylesheet" href="/kit/v1/tokens.css"><link rel="stylesheet" href="/kit/v1/kit.css"><script src="/kit/v1/kit.js"></script><script src="/kit/v1/virtual-list.js"></script>';
  await page.request.put(`/__mock-panes/default/${paneId}`, {
    data: kit + readFileSync(source, "utf8"),
  });
}
