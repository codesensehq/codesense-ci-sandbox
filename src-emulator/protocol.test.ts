// SPDX-License-Identifier: MIT
// @vitest-environment node
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { EmulatorHost } from "./emulator";
import { WORKER_WIRE_VERSION, XTERM_VERSION, type Reply } from "./protocol";

const runtime = { runtime: "bun" as const, version: "1.4.2" };
const request = (value: unknown) => new TextEncoder().encode(JSON.stringify(value));

/** The replies one frame earns, synchronously. */
function answer(payload: Uint8Array, options = {}): Reply[] {
  const replies: Reply[] = [];
  new EmulatorHost((reply) => replies.push(reply), { runtime, ...options }).handleFrame(payload);
  return replies;
}

describe("the worker's request handling", () => {
  it("answers hello with the runtime, the bundled xterm version and the wire version", () => {
    expect(answer(request({ id: 0, type: "hello" }))).toEqual([
      {
        id: 0,
        type: "hello",
        runtime: "bun",
        version: "1.4.2",
        xterm_version: XTERM_VERSION,
        worker_wire_version: WORKER_WIRE_VERSION,
      },
    ]);
  });

  it("reports the exact @xterm/headless version the root package pins", () => {
    const root = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));
    expect(XTERM_VERSION).toBe(root.dependencies["@xterm/headless"]);
  });

  it("answers an unknown type with an error carrying the request id", () => {
    expect(answer(request({ id: 7, type: "snapshot", session: 1 }))).toMatchObject([
      { id: 7, type: "error" },
    ]);
  });

  it("answers a request it must answer but that has no numeric id with an id-less error", () => {
    expect(answer(request({ type: "hello" }))).toMatchObject([{ id: null, type: "error" }]);
    expect(answer(request({ type: "dump", session: 1 }))).toMatchObject([
      { id: null, type: "error" },
    ]);
  });

  it("answers a message for a session it does not hold with an error", () => {
    expect(answer(request({ id: 3, type: "dump", session: 9 }))).toMatchObject([
      { id: 3, type: "error", message: "no session 9" },
    ]);
    expect(answer(request({ type: "feed", session: 9, seq_end: 1, data: "x" }))).toMatchObject([
      { id: null, type: "error" },
    ]);
  });

  it("answers malformed JSON and invalid UTF-8 with an error, never a throw", () => {
    expect(answer(new TextEncoder().encode("{"))[0].type).toBe("error");
    expect(answer(new Uint8Array([0xff, 0xfe]))[0].type).toBe("error");
  });

  it("answers stats only in test mode", () => {
    expect(answer(request({ id: 1, type: "stats" }))).toMatchObject([{ id: 1, type: "error" }]);
    expect(answer(request({ id: 1, type: "stats" }), { stats: true })).toMatchObject([
      { id: 1, type: "stats", sessions: 0 },
    ]);
  });
});
