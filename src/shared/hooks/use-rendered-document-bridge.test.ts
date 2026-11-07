// SPDX-License-Identifier: MIT
import { viewerScript } from "../../../tests/support/page-shim";
import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  useRenderedDocumentBridge,
  registerRenderedDocument,
  runRenderedDocumentRequest,
} from "./use-rendered-document-bridge";

const { listen, resolveReply } = vi.hoisted(() => ({ listen: vi.fn(), resolveReply: vi.fn() }));
vi.mock("@tauri-apps/api/webviewWindow", () => ({ getCurrentWebviewWindow: () => ({ listen }) }));
vi.mock("@/tauri/commands", () => ({
  resolveRenderedDocument: resolveReply,
  admitRenderedDocument: vi.fn(),
}));

const cleanups: (() => void)[] = [];
afterEach(() => {
  cleanups.splice(0).forEach((fn) => fn());
  document.body.innerHTML = "";
  vi.restoreAllMocks();
});

describe("rendered document bridge", () => {
  it("receives requests through the current webview listener and disposes it", async () => {
    const unlisten = vi.fn();
    listen.mockResolvedValue(unlisten);
    resolveReply.mockResolvedValue(undefined);
    const result = { content: "current", capturedAt: "now", truncated: false };
    cleanups.push(registerRenderedDocument("/context/live.html", async () => result));
    const { unmount } = renderHook(useRenderedDocumentBridge);
    expect(listen).toHaveBeenCalledWith("document:read-rendered-request", expect.any(Function));
    listen.mock.lastCall![1]({
      payload: { requestId: "through-event", path: "/context/live.html", format: "text" },
    });
    await waitFor(() => expect(resolveReply).toHaveBeenCalledWith("through-event", result, null));
    unmount();
    await waitFor(() => expect(unlisten).toHaveBeenCalledOnce());
  });

  it("matches normalized paths, rejects ambiguous registrations and unregisters", async () => {
    const capture = vi
      .fn()
      .mockResolvedValue({ content: "ready", capturedAt: "now", truncated: false });
    const unregister = registerRenderedDocument("/context/a/../live.html", capture);
    cleanups.push(unregister);
    const resolve = vi.fn().mockResolvedValue(undefined);
    const request = { requestId: "one", path: "/context/live.html", format: "text" as const };
    await runRenderedDocumentRequest(request, resolve);
    expect(capture).toHaveBeenCalledOnce();
    const second = registerRenderedDocument(request.path, capture);
    cleanups.push(second);
    await runRenderedDocumentRequest(request, resolve);
    expect(resolve).toHaveBeenLastCalledWith("one", null, expect.stringMatching(/Multiple/));
    unregister();
    second();
    await runRenderedDocumentRequest(request, resolve);
    expect(resolve).toHaveBeenLastCalledWith(
      "one",
      null,
      expect.stringMatching(/Open the current HTML document/),
    );
  });
});

// Execute the actual injected shim, so text extraction, cap and archive behavior
// are tested together rather than duplicating the serialization implementation.
function shim() {
  const script = viewerScript("LINK_INTERCEPTOR_JS");
  const handlers: ((event: unknown) => void)[] = [];
  const parent = { postMessage: vi.fn() };
  const windowStub = {
    parent,
    addEventListener: (type: string, fn: (e: unknown) => void) => {
      if (type === "message") handlers.push(fn);
    },
  };
  vi.spyOn(document, "readyState", "get").mockReturnValue("complete");
  const href = "http://localhost:9123/viewer/live.html?cap=secret#anchor";
  new Function("window", "document", "location", "TextEncoder", "TextDecoder", script)(
    windowStub,
    document,
    { href },
    TextEncoder,
    TextDecoder,
  );
  return (format?: string, expectedUrl = href) => {
    handlers.forEach((fn) =>
      fn({
        source: parent,
        data: { type: "codesense:snapshot-request", id: 7, format, expectedUrl },
      }),
    );
    return parent.postMessage.mock.lastCall![0];
  };
}

describe("rendered document iframe serialization", () => {
  it.each(["loading", "interactive"] as const)(
    "rejects formatted reads while %s, preserving legacy snapshots",
    (state) => {
      const capture = shim();
      vi.spyOn(document, "readyState", "get").mockReturnValue(state);
      expect(capture("text")).toMatchObject({
        ok: false,
        error: expect.stringMatching(/still loading/),
      });
      expect(capture("html")).toMatchObject({
        ok: false,
        error: expect.stringMatching(/still loading/),
      });
      expect(capture(undefined, "http://localhost:9123/viewer/before-pushState.html").ok).toBe(
        true,
      );
    },
  );
  it("preserves legacy snapshot after pushState changes the URL", () => {
    const capture = shim();
    expect(capture(undefined, "http://localhost:9123/viewer/before-pushState.html").ok).toBe(true);
  });

  it("uses rendered innerText rather than source textContent", () => {
    document.body.innerHTML = "<p>live</p><span hidden>hidden</span><script>secret</script>";
    Object.defineProperty(document.body, "innerText", { value: "live", configurable: true });
    const result = shim()("text");
    expect(result.content).toBe("live");
    expect(result.truncated).toBe(false);
    expect(Number.isNaN(Date.parse(result.capturedAt))).toBe(false);
  });
  it("strips injected scripts while preserving archive serialization", () => {
    document.body.innerHTML =
      '<p>Live data</p><script>window.cap="secret"</script><template><script>secret</script></template>';
    const capture = shim();
    const html = capture("html");
    expect(html.content).toContain("Live data");
    expect(html.content).not.toContain("secret");
    expect(capture().html).toBe(html.content);
  });
  it("bounds multibyte text without replacement characters and flags truncation", () => {
    Object.defineProperty(document.body, "innerText", {
      value: "€".repeat(200000),
      configurable: true,
    });
    const result = shim()("text");
    expect(new TextEncoder().encode(result.content).length).toBeLessThanOrEqual(524288);
    expect(result.content).not.toContain("�");
    expect(result.truncated).toBe(true);
  });
  it("allows anchor changes but rejects different document navigation", () => {
    const capture = shim();
    expect(capture("text", "http://localhost:9123/viewer/live.html?cap=secret").ok).toBe(true);
    expect(capture("text", "http://localhost:9123/viewer/other.html").ok).toBe(false);
  });
});

it("denies an ancestor-authority viewer before asking it for any DOM", async () => {
  const capture = vi.fn(async () => ({
    content: "PARENT-ONLY-ROWS",
    capturedAt: "now",
    truncated: false,
  }));
  cleanups.push(registerRenderedDocument("/parent/shared.html", capture, "parent-frame"));
  const reply = vi.fn();
  const admit = vi.fn(async () => {
    throw new Error("File not found");
  });
  await runRenderedDocumentRequest(
    { requestId: "child", path: "/parent/shared.html", format: "html", requiresAuthority: true },
    reply,
    admit,
  );
  expect(admit).toHaveBeenCalledWith("child", "parent-frame");
  expect(capture).not.toHaveBeenCalled();
  expect(reply).toHaveBeenCalledWith("child", null, "File not found");
});
