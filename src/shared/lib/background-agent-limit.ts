// SPDX-License-Identifier: MIT
/**
 * SW-DX-17d (AC-3): resolve the effective **background-agent limit** from the
 * user setting. Pure + fail-safe, mirroring `resolveBackgroundSetupCap` — the
 * single owner of "what the setting means as a number" so the wiring hook
 * (`use-background-agent-limit`) and the Settings row's "Auto (N)" hint can never
 * disagree.
 *
 * Unlike the worktree-setup cap, this limit is about **human attention** (how many
 * unattended agents a person can supervise), NOT CPU — so `"auto"` resolves to a
 * fixed sensible default, independent of the machine's core count.
 *
 *   - explicit number  → user override, clamped ≥1 and floored
 *   - "auto"/undefined → {@link DEFAULT_BACKGROUND_AGENT_LIMIT}
 */
export const DEFAULT_BACKGROUND_AGENT_LIMIT = 6;

export function resolveBackgroundAgentLimit(setting: "auto" | number | undefined): number {
  if (typeof setting === "number" && Number.isFinite(setting)) {
    return Math.max(1, Math.floor(setting));
  }
  return DEFAULT_BACKGROUND_AGENT_LIMIT;
}
