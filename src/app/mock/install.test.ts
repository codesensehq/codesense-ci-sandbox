// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { installMockBackend } from "./install";
import { MOCK_ORG_SLUG } from "./fixtures";

// The seam test: exercise the whole mock backend through the SAME door the app
// uses — `window.__TAURI_INTERNALS__.invoke` — after a real install(), not the
// handlers in isolation. This is what proves boot commands and CRUD actually
// reach the store via the shim.
describe("installMockBackend (seam)", () => {
  beforeEach(() => {
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    installMockBackend();
  });

  function internals() {
    return (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
          metadata: { currentWindow: { label: string } };
        };
      }
    ).__TAURI_INTERNALS__;
  }

  it("plants the window/webview metadata the api reads synchronously", () => {
    expect(internals().metadata.currentWindow.label).toBe("main");
  });

  it("serves the boot commands that carry the app past the gate", async () => {
    const profile = (await internals().invoke("local_profile_status")) as {
      active: { principal: string } | null;
    };
    expect(profile.active?.principal).toBeTruthy();

    const orgs = (await internals().invoke("discover_local_orgs", { githubUserId: 1 })) as Array<{
      slug: string;
    }>;
    // SW-UX: the default CodeSense reference org is listed first; the user's org follows.
    expect(orgs[0].slug).toBe("codesense/docs");
    expect(orgs.map((o) => o.slug)).toContain(MOCK_ORG_SLUG);
  });

  it("round-trips a CRUD write through the shim into the shared store", async () => {
    await internals().invoke("write_content_file", { path: "/seam/test.md", content: "# Seam" });

    const details = (await internals().invoke("get_story_details", {
      filePath: "/seam/test.md",
    })) as unknown;
    expect(details).toBeTruthy();
  });

  it("loud-stubs an unmapped business command through the shim", async () => {
    await expect(internals().invoke("totally_unmapped", {})).rejects.toThrow(
      "not mocked: totally_unmapped",
    );
  });
});
