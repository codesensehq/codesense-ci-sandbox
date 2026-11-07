// SPDX-License-Identifier: MIT
// @vitest-environment node
import { describe, expect, it } from "vitest";
import { encodeFrame, FrameReader, MAX_FRAME_BYTES } from "./frame";

const bytes = (text: string) => new TextEncoder().encode(text);

describe("encodeFrame", () => {
  it("prefixes the payload with its big-endian length", () => {
    const frame = encodeFrame(bytes("hi"));
    expect(Array.from(frame)).toEqual([0, 0, 0, 2, 0x68, 0x69]);
  });

  it("encodes an empty payload as a bare header", () => {
    expect(Array.from(encodeFrame(new Uint8Array(0)))).toEqual([0, 0, 0, 0]);
  });
});

describe("FrameReader", () => {
  it("returns every frame of one chunk in order", () => {
    const reader = new FrameReader();
    const chunk = new Uint8Array([...encodeFrame(bytes("a")), ...encodeFrame(bytes("bc"))]);
    expect(reader.push(chunk).map((f) => new TextDecoder().decode(f))).toEqual(["a", "bc"]);
    expect(reader.pendingBytes).toBe(0);
  });

  it("holds a frame split at every byte boundary until it is complete", () => {
    const frame = encodeFrame(bytes('{"id":1,"type":"hello"}'));
    for (let cut = 1; cut < frame.byteLength; cut++) {
      const reader = new FrameReader();
      expect(reader.push(frame.slice(0, cut))).toEqual([]);
      const [whole] = reader.push(frame.slice(cut));
      expect(new TextDecoder().decode(whole)).toBe('{"id":1,"type":"hello"}');
    }
  });

  it("keeps the tail of a following frame", () => {
    const reader = new FrameReader();
    const second = encodeFrame(bytes("second"));
    const chunk = new Uint8Array([...encodeFrame(bytes("first")), ...second.slice(0, 3)]);
    expect(reader.push(chunk)).toHaveLength(1);
    expect(reader.pendingBytes).toBe(3);
    expect(reader.push(second.slice(3))).toHaveLength(1);
  });

  it("rejects a length above the limit instead of buffering it", () => {
    const header = new Uint8Array(4);
    new DataView(header.buffer).setUint32(0, MAX_FRAME_BYTES + 1, false);
    expect(() => new FrameReader().push(header)).toThrow(/exceeds/);
  });
});
