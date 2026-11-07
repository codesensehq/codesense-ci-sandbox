// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useRegisterPanePanels } from "@/features/shell/right-pane/panes/use-register-pane-panels";
import { revealSlot } from "@/features/shell/slots/reveal-slot";
import { useOrgStore } from "@/shared/stores/org-store";
import type { PaneEntry, PaneListing } from "./commands";
import { PANE_OPEN_EVENT, type PaneOpenPayload } from "./events";

const { listeners, listPanes } = vi.hoisted(() => ({
  listeners: new Map<string, Set<(e: { payload: unknown }) => void>>(),
  listPanes: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, cb: (e: { payload: unknown }) => void) => {
    const set = listeners.get(event) ?? new Set();
    set.add(cb);
    listeners.set(event, set);
    return () => set.delete(cb);
  }),
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./commands")>()),
  listPanes,
}));
vi.mock("@/features/shell/slots/reveal-slot", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/features/shell/slots/reveal-slot")>()),
  revealSlot: vi.fn(),
}));

const read = (path: string) => readFileSync(resolve(__dirname, "../..", path), "utf8");

const paneEntry = (id: string): PaneEntry => ({
  id,
  scope: "team",
  team: "team",
  title: id,
  path: `/panes/${id}.html`,
  panesDir: "/panes",
  panelId: `pane:acme:team/${id}`,
});

describe("pane_open intent parity", () => {
  beforeEach(() => {
    listeners.clear();
    vi.mocked(revealSlot).mockClear();
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "team" });
  });
  afterEach(() => useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null }));

  it.each([
    ["inbox", { surface: "inbox" }],
    ["board", { dashboard: "pane:acme:team/board" }],
  ])("hands a pane_open intent for the %s pane to revealSlot", async (id, target) => {
    const listing: PaneListing = {
      contentRoot: "/content",
      panes: [paneEntry(id)],
      chain: [],
    };
    listPanes.mockResolvedValue(listing);
    renderHook(() => useRegisterPanePanels());
    await waitFor(() => expect(listeners.get(PANE_OPEN_EVENT)?.size).toBe(1));
    const intent = { conversation: "Mia", messageId: 7 };
    const payload: PaneOpenPayload = { panelId: `pane:acme:team/${id}`, path: "", intent };
    for (const cb of listeners.get(PANE_OPEN_EVENT)!) cb({ payload });
    await waitFor(() =>
      expect(revealSlot).toHaveBeenCalledWith("right-pane", { ...target, intent }),
    );
  });

  it("carries the optional opaque object through the MCP result and Tauri event into revealSlot", () => {
    const payload: PaneOpenPayload = {
      panelId: "pane:acme:team/inbox",
      path: "/pane/inbox.html",
      intent: { conversation: "Mia", messageId: 7 },
    };
    const rust = read("src-tauri/src/mcp/pane_tools.rs");
    for (const name of ["PaneOpenParams", "PaneOpened"]) {
      const fields = rust.match(new RegExp(String.raw`pub struct ${name} \{([\s\S]*?)\n\}`))?.[1];
      expect(fields).toContain("pub intent: Option<serde_json::Map<String, serde_json::Value>>");
    }
    const handler = read("src-tauri/src/mcp/handler.rs")
      .split("async fn pane_open(")[1]
      .split("async fn notes_list(")[0];
    expect(handler).toContain("params.intent");
    expect(handler).toContain(".emit(crate::panes::PANE_OPEN_EVENT, opened)");
    expect(payload.intent).toEqual({ conversation: "Mia", messageId: 7 });
  });

  it("publishes the channel and size limit in the generated MCP and page references", () => {
    const tools = JSON.parse(
      read("codesense-docs/getting-started/content/generated/mcp-tools.json"),
    ).tools;
    for (const name of ["pane_open", "pane_write"]) {
      const description = tools.find((t: { name: string }) => t.name === name).description;
      expect(description).toContain("codesense.onIntent(cb)");
      expect(description).toContain("4096 UTF-8 bytes");
    }
    expect(read("src-tauri/src/mcp/live_data_authoring.rs")).toContain("codesense:intent");
  });
});
