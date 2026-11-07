// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { selectSessionWorking } from "./use-session-working";
import type { TerminalAiStatus } from "@/shared/stores/viewer-store";

const state = (m: Record<string, TerminalAiStatus>) => ({ aiStatusBySession: m });

describe("selectSessionWorking (SW-DX-10 AC-6)", () => {
  it("is working ONLY while the agent's aiStatus is `running`", () => {
    expect(selectSessionWorking("s1")(state({ s1: "running" }))).toBe(true);
    expect(selectSessionWorking("s1")(state({ s1: "done" }))).toBe(false);
    expect(selectSessionWorking("s1")(state({ s1: "error" }))).toBe(false);
    expect(selectSessionWorking("s1")(state({ s1: "needs-permission" }))).toBe(false);
  });

  it("is idle when the session has no status yet, or no session is bound", () => {
    expect(selectSessionWorking("s1")(state({}))).toBe(false);
    expect(selectSessionWorking(null)(state({ s1: "running" }))).toBe(false);
  });

  it("is session-scoped — another session running does not make this one working", () => {
    expect(selectSessionWorking("s1")(state({ s2: "running" }))).toBe(false);
  });
});
