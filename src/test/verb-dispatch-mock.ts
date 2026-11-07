// SPDX-License-Identifier: MIT
import { vi } from "vitest";
import { VERB_REGISTRY } from "@/features/dashboard/lib/verb-registry.generated";
/** IPC stand-in for the default open policy. Budget and policy behavior is tested in Rust. */
export function openVerbDispatchMock() {
  return {
    verbDispatch: vi.fn(async (_caller: unknown, verb: string) =>
      VERB_REGISTRY.some((v) => v.name === verb)
        ? { status: "admit", callId: "call" }
        : { status: "deny", reason: `denied: ${verb} is not allowlisted` },
    ),
    verbSettle: vi.fn(async () => undefined),
    verbAnswer: vi.fn(),
  };
}
