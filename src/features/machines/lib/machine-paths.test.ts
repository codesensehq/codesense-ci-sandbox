// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { formatMachineAddress, parseMachineAddress } from "./machine-address";
import { machineFilePath, parseMachineFilePath, tildePath } from "./machine-file-path";

describe("parseMachineAddress", () => {
  it.each([
    ["dev@devbox:2222", { username: "dev", host: "devbox", port: 2222 }],
    ["devbox.int", { username: null, host: "devbox.int", port: 22 }],
    ["  dev@devbox  ", { username: "dev", host: "devbox", port: 22 }],
    ["dev@fe80::1", { username: "dev", host: "fe80::1", port: 22 }],
    ["[fe80::1]:2222", { username: null, host: "fe80::1", port: 2222 }],
    ["fe80::1%en0", { username: null, host: "fe80::1%en0", port: 22 }],
    ["2001:db8:0:0:0:0:0:1", { username: null, host: "2001:db8:0:0:0:0:0:1", port: 22 }],
  ])("%s", (input, expected) => {
    expect(parseMachineAddress(input)).toEqual(expected);
  });

  it.each([
    "",
    "dev@",
    "dev@devbox:0",
    "dev@devbox:99999",
    "dev@dev box",
    "host/path",
    "devbox:22:x",
    "dev@devbox::2222",
    "devbox:0x16",
    // Would read as an ssh option in an argv.
    "-oProxyCommand=calc",
    "dev@-oProxyCommand=calc",
    "-oProxyCommand=x@devbox",
    "a lf@devbox",
    // Not an IPv6 address — just colons, or more than one "::".
    "a:b:c",
    "::::",
    ":::",
    "1::2::3",
  ])("rejects %j", (input) => {
    expect(parseMachineAddress(input)).toBeNull();
  });

  it("round-trips through formatMachineAddress (port shown only when non-default)", () => {
    expect(formatMachineAddress({ username: "dev", host: "devbox", port: 22 })).toBe("dev@devbox");
    expect(formatMachineAddress({ username: null, host: "fe80::1", port: 2222 })).toBe(
      "[fe80::1]:2222",
    );
    expect(formatMachineAddress({ username: null, host: "devbox", port: 2222 })).toBe(
      "devbox:2222",
    );
  });
});

describe("machine file paths", () => {
  it("carries the machine in the path and parses back", () => {
    const ref = { machineId: "devbox-01", absPath: "/home/dev/src/server.rs" };
    const path = machineFilePath(ref);
    expect(path).toBe("virtual://machine-file/devbox-01/home/dev/src/server.rs");
    expect(parseMachineFilePath(path)).toEqual(ref);
  });

  it("rejects a malformed machine segment instead of throwing", () => {
    expect(parseMachineFilePath("virtual://machine-file/%E0%A4%A/x")).toBeNull();
  });

  it("never parses a local path as remote", () => {
    expect(parseMachineFilePath("/Users/me/notes.md")).toBeNull();
    expect(parseMachineFilePath("virtual://connections")).toBeNull();
  });

  it("formats display paths", () => {
    expect(tildePath("/home/dev/src/x", "dev")).toBe("~/src/x");
    expect(tildePath("/Users/dev", "dev")).toBe("~");
    expect(tildePath("/srv/app", "dev")).toBe("/srv/app");
    // Another user's home, or an unknown user, stays absolute.
    expect(tildePath("/home/bob/x", "dev")).toBe("/home/bob/x");
    expect(tildePath("/home/dev/x", null)).toBe("/home/dev/x");
    expect(tildePath("/home/devx/x", "dev")).toBe("/home/devx/x");
  });
});
