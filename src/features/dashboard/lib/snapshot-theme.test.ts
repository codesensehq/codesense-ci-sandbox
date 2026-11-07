// SPDX-License-Identifier: MIT
import { afterEach, expect, it } from "vitest";
import { loadShim } from "../../../../tests/support/page-shim";

afterEach(() => {
  document.head.innerHTML = "";
  document.body.innerHTML = "";
  document.documentElement.removeAttribute("style");
  Object.defineProperty(window, "parent", { value: window, configurable: true });
});

it("a snapshot after dark to light removes the original host palette so reopening can use the current theme", () => {
  document.head.innerHTML =
    '<style id="codesense-initial-theme">:root:root{--cs-bg:#161b22;--cs-fg:#ffffff;color-scheme:dark}</style><style>body{background:var(--cs-bg);color:var(--cs-fg)}</style>';
  document.body.innerHTML = "<p>Useful content</p>";
  const { deliver, posted } = loadShim();
  deliver({
    type: "codesense:theme",
    theme: { scheme: "dark", tokens: { "--cs-bg": "#161b22", "--cs-fg": "#ffffff" } },
  });
  deliver({
    type: "codesense:theme",
    theme: { scheme: "light", tokens: { "--cs-bg": "#ffffff", "--cs-fg": "#161b22" } },
  });
  deliver({ type: "codesense:snapshot-request", id: 7 });
  const capture = posted.find(
    (message) => message.type === "codesense:snapshot-result" && message.id === 7,
  )!;
  expect(capture.ok).toBe(true);
  const html = String(capture.html);
  expect(html).not.toContain("codesense-initial-theme");
  expect(html).not.toContain("#161b22");
  expect(html).toContain("background:var(--cs-bg)");
  expect(document.getElementById("codesense-initial-theme")).not.toBeNull();
  // Reopen the serialized bytes under the current host's first-paint palette.
  const reopened = new DOMParser().parseFromString(
    '<style id="codesense-initial-theme">:root:root{--cs-bg:#ffffff;--cs-fg:#161b22;color-scheme:light}</style>' +
      html,
    "text/html",
  );
  expect(reopened.querySelectorAll("#codesense-initial-theme")).toHaveLength(1);
  expect(reopened.querySelector("#codesense-initial-theme")?.textContent).toContain(
    "color-scheme:light",
  );
});
