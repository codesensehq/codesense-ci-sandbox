// SPDX-License-Identifier: MIT
import { afterEach, expect, it, vi } from "vitest";
import { deliverMenuAction } from "./menu-action-delivery";
afterEach(() => vi.restoreAllMocks());
it("waits for the same frame's acknowledgement before permitting the unread write", async () => {
  const postMessage = vi.fn();
  const child = { postMessage } as unknown as Window;
  const promise = deliverMenuAction(
    { contentWindow: child, src: "http://frame.test/pane/inbox" },
    { type: "codesense:menu-action", itemId: "mark-unread" },
    new AbortController().signal,
  );
  const id = postMessage.mock.calls[0][0].id;
  let done = false;
  void promise.then(() => {
    done = true;
  });
  window.dispatchEvent(
    new MessageEvent("message", {
      origin: "http://other.test",
      source: child,
      data: { type: "codesense:menu-action-ack", id },
    }),
  );
  await Promise.resolve();
  expect(done).toBe(false);
  window.dispatchEvent(
    new MessageEvent("message", {
      origin: "http://frame.test",
      source: child,
      data: { type: "codesense:menu-action-ack", id },
    }),
  );
  await promise;
  expect(done).toBe(true);
});
it("cancels delivery when its frame reloads", async () => {
  const controller = new AbortController();
  const promise = deliverMenuAction(
    { contentWindow: { postMessage: vi.fn() } as unknown as Window, src: "http://frame.test/pane" },
    {},
    controller.signal,
  );
  controller.abort();
  await expect(promise).rejects.toThrow("Menu frame closed");
});
