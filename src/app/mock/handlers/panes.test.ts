// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PaneListing } from "@/tauri/commands";
import { MockEventBus } from "../mock-events";
import { paneHandlers } from "./panes";

afterEach(() => vi.unstubAllGlobals());

describe("mock pane discovery", () => {
  it("reports a declared surface in any scope but keeps team panes team-scoped, like the Rust discovery", async () => {
    const marked =
      '<meta name="codesense:icon" content="inbox"><META content=human-inbox NAME="codesense:surface">';
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        json: async () => [
          { team: "default", id: "team-inbox", html: marked, modified: "now" },
          { team: "-", id: "my-inbox", html: marked, modified: "now" },
          { team: "-", id: "plain", html: "<title>Plain</title>", modified: "now" },
        ],
      })),
    );
    const handlers = paneHandlers(new MockEventBus());
    const listing = (await handlers.list_panes({ org: "acme", team: "default" })) as PaneListing;
    expect(listing.panes.find((p) => p.id === "my-inbox")).toMatchObject({
      scope: "private",
      surface: "human-inbox",
      icon: "inbox",
    });
    // A team pane keeps its declared surface, but it stays a team pane: only Rust's
    // `private_reach` policy may install it privately, and the listing never does.
    expect(listing.panes.find((p) => p.id === "team-inbox")).toMatchObject({
      scope: "team",
      team: "default",
      surface: "human-inbox",
      panelId: "pane:acme:default/team-inbox",
    });
    expect(listing.panes.find((p) => p.id === "plain")?.surface).toBeUndefined();
  });
});
