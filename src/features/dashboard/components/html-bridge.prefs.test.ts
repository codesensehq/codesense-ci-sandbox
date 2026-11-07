// SPDX-License-Identifier: MIT
/**
 * SW-DNS-2: the `prefs` / `setPrefs` bridge verbs act on the calling dashboard's
 * own entry, named by the host (`ctx.prefsKey`), never by the doc.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

const { store } = vi.hoisted(() => ({ store: { raw: "{}" } }));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  loadAppState: vi.fn(async () => store.raw),
  saveAppState: vi.fn(async (s: string) => {
    store.raw = s;
  }),
}));

import { ACTION_VERBS, READ_VERBS, runBridgeCommand } from "./html-bridge";

beforeEach(() => {
  store.raw = "{}";
});

describe("prefs bridge verbs (SW-DNS-2)", () => {
  it("prefs is a read; setPrefs is a metadata-class write (rate-guarded)", () => {
    expect(READ_VERBS).toContain("prefs");
    expect(ACTION_VERBS).toContain("setPrefs");
  });

  it("each doc reads and writes only its own entry; a doc-supplied key is ignored", async () => {
    const team = { prefsKey: "pane:/ctx/acme/a/panes/pipeline.html" };
    const priv = { prefsKey: "pane:/scratch/orgs/acme/panes/pipeline.html" };
    await runBridgeCommand("setPrefs", { details: "compact", key: "pane:x" }, team);
    await runBridgeCommand("setPrefs", { details: "full" }, priv);
    expect(await runBridgeCommand("prefs", { key: priv.prefsKey }, team)).toEqual({
      details: "compact",
      key: "pane:x",
    });
    expect(await runBridgeCommand("prefs", undefined, priv)).toEqual({ details: "full" });
  });

  it("a caller with no dashboard identity is refused", async () => {
    await expect(runBridgeCommand("prefs", undefined, {})).rejects.toThrow(/^unavailable:/);
  });
});
