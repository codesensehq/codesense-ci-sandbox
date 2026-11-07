// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";

import { logger } from "@/shared/lib/logger";
import {
  agentPinId,
  agentRowKey,
  sanitizeAgentPinMap,
  sanitizeAgentPins,
  sessionPinId,
} from "./agent-pins";

describe("agent row and pin keys", () => {
  it("keep a team key with a colon apart from a handle with one", () => {
    expect(agentRowKey("a:b", "c")).not.toBe(agentRowKey("a", "b:c"));
    expect(sessionPinId("a:b", "c")).not.toBe(sessionPinId("a", "b:c"));
  });

  it("keep two saved pins whose team and handle only join alike", () => {
    const pins = sanitizeAgentPins([
      { kind: "session", teamKey: "a:b", handle: "c", sessionId: null, name: "one" },
      { kind: "session", teamKey: "a", handle: "b:c", sessionId: null, name: "two" },
    ]);
    expect(pins.map((p) => p.name)).toEqual(["one", "two"]);
    expect(new Set(pins.map(agentPinId)).size).toBe(2);
  });

  it("keep one saved pin of a session: a later pin of the same session id is dropped", () => {
    const pins = sanitizeAgentPins([
      { kind: "session", teamKey: "t", handle: "a", sessionId: "s1", name: "first" },
      { kind: "session", teamKey: "t", handle: "b", sessionId: "s1", name: "again" },
      { kind: "session", teamKey: "t", handle: "c", sessionId: null, name: "no id" },
      { kind: "session", teamKey: "t", handle: "d", sessionId: null, name: "no id either" },
    ]);
    expect(pins.map((p) => p.name)).toEqual(["first", "no id", "no id either"]);
  });

  it("keep a session pinned in two teams' lists only in the first", () => {
    const map = sanitizeAgentPinMap({
      "acme:ops": [
        { kind: "session", teamKey: "acme:ops", handle: "far", sessionId: "s1", name: "" },
      ],
      "team-1": [
        { kind: "session", teamKey: "team-1", handle: "far", sessionId: "s1", name: "" },
        { kind: "session", teamKey: "team-1", handle: "near", sessionId: "s2", name: "" },
      ],
      junk: "not a list",
    });
    expect(
      Object.fromEntries(Object.entries(map).map(([k, v]) => [k, v.map((p) => p.handle)])),
    ).toEqual({ "acme:ops": ["far"], "team-1": ["near"] });
    expect(sanitizeAgentPinMap(null)).toEqual({});
  });

  it("keep no entry for a team left with no pins, and log a dropped second pin", () => {
    const debug = vi.spyOn(logger, "debug").mockImplementation(() => {});
    try {
      const pin = {
        kind: "session",
        teamKey: "acme:ops",
        handle: "far",
        sessionId: "s1",
        name: "",
      };
      const map = sanitizeAgentPinMap({ "acme:ops": [pin, pin], "acme:core": [], empty: [] });
      expect(Object.keys(map)).toEqual(["acme:ops"]);
      expect(debug.mock.calls.filter(([, msg]) => msg.includes("acme:ops far"))).toHaveLength(1);
    } finally {
      debug.mockRestore();
    }
  });

  it("keep only the pins of the team whose list holds them", () => {
    const map = sanitizeAgentPinMap({
      "acme:core": [
        { kind: "session", teamKey: "acme:ops", handle: "far", sessionId: "s1", name: "" },
        { kind: "session", teamKey: "acme:core", handle: "near", sessionId: "s2", name: "" },
      ],
      "acme:ops": [
        { kind: "session", teamKey: "acme:ops", handle: "far", sessionId: "s1", name: "" },
      ],
    });
    expect(map["acme:core"].map(agentPinId)).toEqual([sessionPinId("acme:core", "near")]);
    // The pin the other list held under the wrong team does not keep its own team's pin out.
    expect(map["acme:ops"].map(agentPinId)).toEqual([sessionPinId("acme:ops", "far")]);
  });
});
