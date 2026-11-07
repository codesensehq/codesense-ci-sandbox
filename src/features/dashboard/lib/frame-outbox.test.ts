// SPDX-License-Identifier: MIT
import { expect, it, vi } from "vitest";
import { FrameOutbox } from "./frame-outbox";

it("queues before handshake, flushes in order, and discards outgoing-document messages", () => {
  const box = new FrameOutbox();
  const postMessage = vi.fn();
  const frame = {
    src: "http://f-a.localhost:9321/pane/a",
    contentWindow: { postMessage } as unknown as Window,
  };
  box.begin("frame-a/document-a");
  box.post(frame, { type: "first" });
  box.post(frame, { type: "second" });
  expect(postMessage).not.toHaveBeenCalled();
  box.ready(frame);
  expect(postMessage.mock.calls.map(([message]) => message.type)).toEqual(["first", "second"]);
  expect(postMessage.mock.calls.every(([, origin]) => origin === "http://f-a.localhost:9321")).toBe(
    true,
  );
  box.begin("frame-a/document-a");
  box.post(frame, { type: "same-document" });
  expect(postMessage).toHaveBeenCalledTimes(3);
  box.begin("frame-a/document-b");
  box.post(frame, { type: "outgoing" });
  box.begin("frame-a/document-c");
  box.ready(frame);
  expect(postMessage).toHaveBeenCalledTimes(3);
});

it("a candidate's handshake cannot release the visible frame's queue", () => {
  const visible = new FrameOutbox(),
    candidate = new FrameOutbox();
  const frame = {
    src: "http://f-a.localhost:9321/pane/a",
    contentWindow: { postMessage: vi.fn() } as unknown as Window,
  };
  visible.begin("visible");
  candidate.begin("candidate");
  visible.post(frame, { type: "waiting" });
  candidate.ready(frame);
  expect(frame.contentWindow.postMessage).not.toHaveBeenCalled();
  visible.ready(frame);
  expect(frame.contentWindow.postMessage).toHaveBeenCalledOnce();
});
