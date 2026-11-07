// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { readFileSync } from "fs";
import { resolve } from "path";

describe("tauri.conf.json", () => {
  const config = JSON.parse(
    readFileSync(resolve(__dirname, "../src-tauri/tauri.conf.json"), "utf-8"),
  );
  const windowConfig = config.app.windows[0];

  it("has titleBarStyle set to Overlay for native macOS titlebar", () => {
    expect(windowConfig.titleBarStyle).toBe("Overlay");
  });

  it("has acceptFirstMouse enabled", () => {
    expect(windowConfig.acceptFirstMouse).toBe(true);
  });

  it("allows HTTP for per-frame localhost origins in the macOS bundle", () => {
    const plist = new DOMParser().parseFromString(
      readFileSync(resolve(__dirname, "../src-tauri/Info.plist"), "utf-8"),
      "application/xml",
    );
    const value = (dict: Element | null | undefined, key: string) =>
      Array.from(dict?.children ?? []).find(
        (child) => child.localName === "key" && child.textContent === key,
      )?.nextElementSibling;
    const ats = value(plist.querySelector("plist > dict"), "NSAppTransportSecurity");
    const localhost = value(value(ats, "NSExceptionDomains"), "localhost");

    // WKWebView's per-frame origins (http://f-<id>.localhost) need an ATS
    // exception that includes *.localhost, beyond the bare loopback host.
    expect(value(localhost, "NSIncludesSubdomains")?.localName).toBe("true");
    expect(value(localhost, "NSExceptionAllowsInsecureHTTPLoads")?.localName).toBe("true");
  });
});
