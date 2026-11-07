// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { rendererPrincipal } from "@/shared/lib/local-profile-session";
import { verbDispatch, type VerbCaller } from "./verb-dispatch";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: vi.fn() }));
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(rendererPrincipal).mockResolvedValue("42");
  vi.mocked(invoke).mockResolvedValue({ status: "admit", callId: "call" });
});
it.each(["human", "reviewer", "role:lead"])(
  "keeps frame actor %s separate from the immutable policy profile",
  async (principal) => {
    const caller: VerbCaller = {
      kind: "message",
      frameId: "frame-1",
      principal,
      dashboard: "inbox-message://acme/team/7",
      slot: "message",
      team: { org: "acme", team: "team" },
      scope: "open",
    };
    await expect(verbDispatch(caller, "sessions", null)).resolves.toEqual({
      status: "admit",
      callId: "call",
    });
    expect(invoke).toHaveBeenCalledExactlyOnceWith("verb_dispatch", {
      caller,
      expectedPrincipal: "42",
      verb: "sessions",
      args: {},
    });
  },
);
