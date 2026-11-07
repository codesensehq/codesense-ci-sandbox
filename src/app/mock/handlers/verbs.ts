// SPDX-License-Identifier: MIT
import { VERB_REGISTRY } from "@/features/dashboard/lib/verb-registry.generated";
import type { HandlerRegistry } from "../mock-router";
/** Mock mode has no native dispatcher. Its default open policy never prompts. */
export const verbHandlers: HandlerRegistry = {
  verb_dispatch: ({ verb }) =>
    VERB_REGISTRY.some((v) => v.name === verb)
      ? { status: "admit", callId: crypto.randomUUID() }
      : { status: "deny", reason: "denied: not-allowed" },
  verb_settle: () => null,
};
