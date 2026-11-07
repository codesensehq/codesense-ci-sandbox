// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { expect, it, vi } from "vitest";
const call = vi.hoisted(() => vi.fn());
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  fileSurfaceCall: call,
}));
import { HTML_BRIDGE_ALLOWLIST as bridge } from "./html-bridge";
import { pageSdk } from "../lib/page-sdk";

for (const [operation, twin, args, result] of [
  [
    "list",
    "list_context_content",
    { scope: "team" },
    { rows: [{ id: "/own/a.md", path: "/own/a.md", name: "a.md", kind: "file" }] },
  ],
  [
    "read",
    "read_content_file",
    { path: "/own/a.md" },
    { text: "hello", size: 5, truncated: false, sha256: "hash" },
  ],
  [
    "move",
    "move_content_file",
    { from: "/own/a.md", to: "/own/b.md" },
    { fromPath: "/own/a.md", toPath: "/own/b.md" },
  ],
  ["delete", "delete_content_file", { path: "/own/a.md" }, { deletedPath: "/own/a.md" }],
  [
    "create",
    "create_content_file",
    { path: "/own/a.md", content: "hello", onCollision: "fail" },
    { path: "/own/a.md" },
  ],
] as const) {
  it(`files.${operation} is discoverable through its existing capability grant`, () => {
    const capability = operation === "list" || operation === "read" ? "files:read" : "files:write";
    expect(pageSdk({ effectiveGrant: [capability] }).verbs).toContain(`files.${operation}`);
    expect(pageSdk({ effectiveGrant: [] }).verbs).not.toContain(`files.${operation}`);
  });
  it(`files.${operation} returns the shared Rust result used by ${twin}`, async () => {
    call.mockResolvedValueOnce(result);
    const returned = await bridge[`files.${operation}`](args, {
      frame: { frameId: "bound-frame" },
    } as any);
    expect(call).toHaveBeenLastCalledWith(operation, args, "bound-frame");
    expect(returned).toBe(result);
    const source = readFileSync("src-tauri/src/mcp/handler.rs", "utf8");
    const body = source.split(`async fn ${twin}(`)[1].split("\n    }")[0];
    expect(body).toContain("commands::file_surface::call");
    expect(body).toContain(`"${operation}"`);
  });
}
