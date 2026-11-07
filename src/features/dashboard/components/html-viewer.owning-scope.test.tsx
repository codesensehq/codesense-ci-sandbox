// SPDX-License-Identifier: MIT
import { beforeEach, afterEach, it, expect, vi } from "vitest";
import { render, waitFor, cleanup, act } from "@testing-library/react";
const { ipc } = vi.hoisted(() => ({ ipc: vi.fn() }));
vi.mock("@tauri-apps/api/core", async (original) => ({
  ...(await original<typeof import("@tauri-apps/api/core")>()),
  invoke: (command: string, args: unknown) => ipc(command, args),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}), emit: vi.fn() }));
import { HtmlViewer } from "./html-viewer";
import { OwningScopeProvider } from "@/shared/lib/warm-scopes";
import { hasLiveFrames } from "@/features/dashboard/lib/frame-lifecycle";
import { liveHub } from "@/features/dashboard/lib/live-hub";

// Seam: the real HtmlViewer and frame registry; only the Tauri IPC is stubbed. The grant names a
// team other than the one whose view mounts the frame, as an inherited pane's or a rebinding
// dock's does.
const record = {
  kind: "private",
  documentPath: "/ctx/content/board.html",
  frameId: "fresh",
  origin: "http://f-fresh.localhost:9321",
  token: "frame-secret",
  grant: {
    frameId: "fresh",
    principal: "human",
    dashboard: "/ctx/content/board.html",
    slot: "centre",
    team: { org: "acme", team: "source" },
    scope: "open",
  },
};
beforeEach(() => {
  ipc.mockImplementation(async (command: string) => {
    if (command === "get_http_api_port") return 9321;
    if (command === "local_profile_status") return { active: { principal: "human" } };
    if (command === "get_proxy_capability") return "process-secret";
    if (command === "mint_frame") return record;
    if (command === "resolve_frame_grant") return record.grant;
    if (command === "live_hub_view_key") return "all";
    return null;
  });
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: true, status: 200 })),
  );
});
afterEach(() => {
  cleanup();
  liveHub.dispose();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

async function mountFrame(keepTeam: boolean) {
  render(
    <OwningScopeProvider scope="acme:shown">
      <HtmlViewer
        filePath={record.documentPath}
        teamContentPath="/ctx/content"
        keepTeam={keepTeam}
        visible
      />
    </OwningScopeProvider>,
  );
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(record.origin));
  const frame = document.querySelector("iframe")!;
  Object.defineProperty(frame, "contentWindow", {
    value: { postMessage: vi.fn() },
    configurable: true,
  });
  act(() => {
    window.dispatchEvent(
      new MessageEvent("message", {
        source: frame.contentWindow,
        origin: record.origin,
        data: { type: "codesense:ready", token: record.token },
      }),
    );
  });
}

it("registers a frame under the team whose view mounts it, never its grant's team", async () => {
  await mountFrame(false);
  await waitFor(() => expect(hasLiveFrames((f) => f.teamKey === "acme:shown")).toBe(true));
  expect(hasLiveFrames((f) => f.teamKey === "acme:source")).toBe(false);
});

it("keeps a frame held for another org's dock bound to its own team", async () => {
  await mountFrame(true);
  await waitFor(() => expect(hasLiveFrames((f) => f.teamKey === "acme:source")).toBe(true));
  expect(hasLiveFrames((f) => f.teamKey === "acme:shown")).toBe(false);
});
