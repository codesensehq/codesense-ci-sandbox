// SPDX-License-Identifier: MIT
import { beforeEach, afterEach, it, expect, vi } from "vitest";
import { render, waitFor, cleanup, act } from "@testing-library/react";
const { ipc } = vi.hoisted(() => ({ ipc: vi.fn() }));
vi.mock("@tauri-apps/api/core", async (original) => ({
  ...(await original<typeof import("@tauri-apps/api/core")>()),
  invoke: (command: string, args: unknown) => ipc(command, args),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}), emit: vi.fn() }));
vi.mock("@/shared/lib/logger", () => ({
  logger: { error: vi.fn(), warn: vi.fn(), info: vi.fn(), debug: vi.fn(), trace: vi.fn() },
}));
import { HtmlViewer } from "./html-viewer";
import { liveHub } from "@/features/dashboard/lib/live-hub";

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
    team: null,
    scope: "open",
  },
};
const FILE = record.documentPath;
let admittedReads = 0;
beforeEach(() => {
  admittedReads = 0;
  ipc.mockImplementation(async (command: string) => {
    if (command === "get_http_api_port") return 9321;
    if (command === "local_profile_status") return { active: { principal: "human" } };
    if (command === "get_proxy_capability") return "process-secret";
    if (command === "mint_frame") return record;
    if (command === "resolve_frame_grant") return record.grant;
    if (command === "live_hub_view_key") return "all";
    if (command === "verb_dispatch") return { status: "admit", callId: "c" };
    if (command === "query_context") {
      admittedReads += 1;
      return [];
    }
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

const view = (visible: boolean, refreshKey = 0) => (
  <HtmlViewer
    filePath={FILE}
    teamContentPath="/ctx/content"
    visible={visible}
    refreshKey={refreshKey}
  />
);
async function mount(visible: boolean) {
  const utils = render(view(visible));
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain(record.origin));
  return utils;
}
function hook(frame: HTMLIFrameElement) {
  const postMessage = vi.fn();
  const source = { postMessage } as unknown as Window;
  Object.defineProperty(frame, "contentWindow", { value: source, configurable: true });
  const send = (data: Record<string, unknown>) =>
    act(() => {
      window.dispatchEvent(
        new MessageEvent("message", {
          source,
          origin: record.origin,
          data: { token: record.token, ...data },
        }),
      );
    });
  const types = () => postMessage.mock.calls.map(([m]) => m.type);
  return { postMessage, send, types };
}
const read = (id: number) => ({ type: "codesense:invoke", id, command: "queryContext", args: {} });
const results = (postMessage: ReturnType<typeof vi.fn>) =>
  postMessage.mock.calls.map(([m]) => m).filter((m) => m.type === "codesense:invoke-result");

it("a hidden reload holds reads, then one flush on show answers every invoke", async () => {
  const { rerender } = await mount(false);
  rerender(view(false, 1));
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain("v="));
  const frame = document.querySelector("iframe")!;
  const page = hook(frame);
  page.send({ type: "codesense:ready" });
  page.send(read(1));
  page.send(read(2));
  await act(async () => {});
  expect(admittedReads).toBe(0);
  expect(results(page.postMessage)).toHaveLength(0);
  rerender(view(true, 1));
  await waitFor(() => expect(results(page.postMessage)).toHaveLength(2));
  expect(
    results(page.postMessage)
      .map((m) => m.id)
      .sort(),
  ).toEqual([1, 2]);
  expect(results(page.postMessage).every((m) => m.ok === true)).toBe(true);
  expect(admittedReads).toBe(1);
});

it("a page whose ready was lost is asked to say it again on show, and the repeat is idempotent", async () => {
  const { rerender } = await mount(false);
  const page = hook(document.querySelector("iframe")!);
  expect(page.types()).not.toContain("codesense:hello");
  rerender(view(true));
  await waitFor(() => expect(page.types()).toContain("codesense:hello"));
  expect(page.postMessage).toHaveBeenCalledWith({ type: "codesense:hello" }, record.origin);
  page.send({ type: "codesense:ready", retry: true });
  await waitFor(() => expect(page.types()).toContain("codesense:theme"));
  const posted = page.postMessage.mock.calls.length;
  page.send({ type: "codesense:ready", retry: true });
  await act(async () => {});
  expect(page.postMessage.mock.calls.length).toBe(posted);
});

it("switching away and back never reloads the document and sends no request on return", async () => {
  const { rerender } = await mount(true);
  const frame = document.querySelector("iframe")!;
  const page = hook(frame);
  page.send({ type: "codesense:ready" });
  await waitFor(() => expect(page.types()).toContain("codesense:visibility"));
  const src = frame.src;
  ipc.mockClear();
  page.postMessage.mockClear();
  rerender(view(false));
  rerender(view(true));
  await act(async () => {});
  expect(document.querySelector("iframe")).toBe(frame);
  expect(frame.src).toBe(src);
  expect(page.types()).not.toContain("codesense:hello");
  expect(ipc).not.toHaveBeenCalled();
  expect(admittedReads).toBe(0);
});

it("a call from a replaced document is refused before it runs", async () => {
  let release!: () => void;
  const base = ipc.getMockImplementation()!;
  ipc.mockImplementation(async (command, args) => {
    if (command === "verb_dispatch") {
      await new Promise<void>((resolve) => (release = resolve));
      return { status: "admit", callId: "slow" };
    }
    return base(command, args);
  });
  const { rerender } = await mount(true);
  const page = hook(document.querySelector("iframe")!);
  page.send({ type: "codesense:ready" });
  page.send(read(9));
  await waitFor(() => expect(release).toBeTypeOf("function"));
  rerender(view(true, 1));
  await waitFor(() => expect(document.querySelector("iframe")?.src).toContain("v="));
  release();
  await act(async () => {});
  expect(admittedReads).toBe(0);
});
