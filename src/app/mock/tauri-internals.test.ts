// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { buildTauriInternals } from "./tauri-internals";

describe("buildTauriInternals", () => {
  it("exposes the window + webview labels the api reads synchronously (getCurrentWindow/Webview)", () => {
    const internals = buildTauriInternals(() => Promise.resolve());
    expect(internals.metadata.currentWindow.label).toBe("main");
    expect(internals.metadata.currentWebview.label).toBe("main");
  });

  it("delegates invoke to the supplied router", async () => {
    const router = vi.fn(() => Promise.resolve("routed"));
    const internals = buildTauriInternals(router);
    await expect(internals.invoke("load_orgs", { a: 1 })).resolves.toBe("routed");
    expect(router).toHaveBeenCalledWith("load_orgs", { a: 1 });
  });

  it("hands back a distinct numeric id per transformCallback and tolerates unregister", () => {
    const internals = buildTauriInternals(() => Promise.resolve());
    const id1 = internals.transformCallback(() => {}, false);
    const id2 = internals.transformCallback(() => {}, false);
    expect(id1).not.toBe(id2);
    expect(() => internals.unregisterCallback(id1)).not.toThrow();
  });

  it("convertFileSrc returns a usable string rather than throwing", () => {
    const internals = buildTauriInternals(() => Promise.resolve());
    expect(typeof internals.convertFileSrc("/tmp/x.png")).toBe("string");
  });
});
