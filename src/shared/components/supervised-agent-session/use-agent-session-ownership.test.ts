// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";

import { useAgentSessionOwnership } from "./use-agent-session-ownership";

/**
 * SW-WV-A1 (AC-5) — two-axis authority: the scope lease (`wv_lease`) and pane
 * ownership are ORTHOGONAL. This hook is pure given its binding (no internal
 * state, no React context — it reads props and returns a derived surface), so
 * it is exercised as a plain function call. The point of these tests: no matter
 * whether a root lease exists (a fact this hook never sees), a `user`-owned pane
 * stays human-driven — the lease axis cannot flip the pane axis to `agent`.
 */
describe("useAgentSessionOwnership — pane ownership is orthogonal to the scope lease", () => {
  const noop = vi.fn();

  it("root-lease holder with user-owned pane — human keeps the keyboard", () => {
    // A session that holds the fleet's root lease but where the HUMAN has taken
    // the wheel of this worker pane (owner: "user"). The hook has no knowledge of
    // the lease; it derives purely from the pane binding. `inputLocked` means
    // "user input is locked OUT because the agent holds the keyboard" — so a
    // user-owned pane is NOT locked (the human types), and no lease can change that.
    const api = useAgentSessionOwnership({
      sessionId: "s1",
      owner: "user",
      connection: "live",
      onDisconnect: noop,
      onReconnect: noop,
    });
    expect(api.inputLocked).toBe(false);
    expect(api.isDriving).toBe(false);
    // The human holds the keyboard on this worker even though (elsewhere) the
    // controller holds the fleet lease — the two axes do not interfere.
  });

  it("agent-owned live pane drives regardless of any lease", () => {
    const api = useAgentSessionOwnership({
      sessionId: "s1",
      owner: "agent",
      connection: "live",
      onDisconnect: noop,
      onReconnect: noop,
    });
    expect(api.inputLocked).toBe(true);
    expect(api.isDriving).toBe(true);
  });
});
