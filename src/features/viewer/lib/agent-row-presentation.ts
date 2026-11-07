// SPDX-License-Identifier: MIT
import { splitStoryHandle, titleCaseLabel } from "@/features/dashboard/components/dashboard-launch";
import { CODESENSE_LAUNCHER } from "@/shared/stores/viewer-store";
import type { AgentSessionRow } from "@/tauri/commands";
import { useAgentListProjectionStore } from "./agent-list-projection";

// ---------------------------------------------------------------------------
// Pure agent-row display helpers — shared by the agents list, the run output and
// the recent-runs list, so every surface renders a session row the SAME way. No
// React, no store, no I/O — every helper is unit-tested without a render.
// ---------------------------------------------------------------------------

/** SW-OR-6 (AC-3): a headless run is the PTY-less `run_agent` pseudo-session
 *  (SW-OR-10, registered `kind: "headless"` = the Rust `KIND_HEADLESS`). It has no
 *  TTY child to attach, so no surface may offer it a TTY-foreground affordance —
 *  that would point an xterm at a childless `sessionId`, yielding a dead / spinning
 *  terminal tab. This ONE predicate owns the kind literal; it is enforced at every
 *  seam that could reach `foregroundSessionId` (the panel + run-monitor buttons AND
 *  the Show resolver `resolveAgentShow`). Takes just `{ kind }` so the pure
 *  `resolveAgentShow` resolver can reuse it. Pure + exported → unit-tested without a render. */
export function isHeadlessSession(session: Pick<AgentSessionRow, "kind">): boolean {
  return session.kind === "headless";
}

/** SW-DX-17a (AC-7): the running session to bring to the FOREGROUND as a
 *  visible CLI tab — its host places that ALREADY-RUNNING session as a tab, so
 *  the xterm attaches to its retained replay buffer — or `null` when there is
 *  none to show. Pure + exported so the mapping is unit-tested without a render. */
export function foregroundSessionId(session: AgentSessionRow): string | null {
  if (!session.sessionId) return null; // a reserved-but-unbound launch has no PTY.
  // SW-OR-6 (AC-3): a headless run is PTY-LESS — it has a bound sessionId but no TTY
  // child to attach, so it must never become a foreground tab (→ dead/spinning tab).
  // The button + chip-drill are guarded upstream; this is the seam-level backstop so
  // NO caller can foreground a headless run, present or future.
  if (isHeadlessSession(session)) return null;
  return session.sessionId;
}

/** A compact display label for an opaque handle (storyId / epicDocPath /
 *  `team::epic` / `repo::branch`) — the last path/`::` segment, never parsed for
 *  meaning (handles stay opaque per SW-DX-11 §2). */
function shortHandle(handle: string): string {
  const tail = handle.split("::").pop() ?? handle;
  return tail.split("/").filter(Boolean).pop() ?? handle;
}

/** SW-DX-17e (AC-6): render a COMPOUND session handle `stem#label` — a SW-GW-6
 *  named session or a `sessionMode:'new'` fork like `SW-DX-3#attempt-2` — as
 *  "stem · Label" so its ORIGIN story stem stays visible and the instance is
 *  legible, never a bare opaque `storyId#attempt-2`. A plain handle (no `#`)
 *  falls back to {@link shortHandle}, byte-identical to before. Reuses the ONE
 *  canonical handle split + label humanizer (SW-GW-6/11), so this display can
 *  never drift from how the handle was minted. Pure + exported → unit-tested
 *  without a render. */
export function formatSessionHandle(handle: string): string {
  // Only a STORY session handle uses the `storyId#label` grammar — a bare storyId
  // (a slug) contains neither `::` nor `/`. Everything else is opaque and renders
  // via {@link shortHandle}: a branch/team handle (`::`-bearing, `repo::branch`) or
  // a PATH handle (epic `…/README.md`, dir absolute path — both may legally contain
  // `#`). Guarding on `::`/`/` (not just `::`) stops story grammar from mis-splitting
  // a path like `/x/my#proj` into "my · Proj". A multi-`#` story handle also stays
  // opaque (never leak a raw `#` into the humanized label). Review r1+r2.
  if (handle.includes("::") || handle.includes("/")) return shortHandle(handle);
  const { storyId, label } = splitStoryHandle(handle);
  if (!label || label.includes("#")) return shortHandle(handle);
  return `${shortHandle(storyId)} · ${titleCaseLabel(label)}`;
}

/** Session naming inputs projected once for every agent surface. */
export function agentRowNames(
  session: Pick<AgentSessionRow, "handle"> &
    Partial<Pick<AgentSessionRow, "displayName" | "rowNames">>,
): NonNullable<AgentSessionRow["rowNames"]> {
  const resolved =
    session.rowNames ??
    useAgentListProjectionStore
      .getState()
      .sessions?.find((row) => row.handle === session.handle || row.sessionId === session.handle)
      ?.rowNames;
  if (resolved) return resolved;
  const primary = session.displayName || formatSessionHandle(session.handle);
  return { primary, secondary: null, full: primary };
}

/** Compact primary label for surfaces that need a single string. */
export function agentRowLabel(session: Parameters<typeof agentRowNames>[0]): string {
  return agentRowNames(session).primary;
}

/** The compact seconds→`Ns`/`Nm`/`Nh Mm` bucket ladder behind {@link formatElapsed}.
 *  Assumes a non-negative span (the caller owns the sign guard). */
function formatDurationSecs(secs: number): string {
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m`;
  const hrs = Math.floor(mins / 60);
  const remMins = mins % 60;
  return remMins > 0 ? `${hrs}h ${remMins}m` : `${hrs}h`;
}

/** SW-DX-17d (AC-2): a compact "elapsed since launch" label from an epoch-ms
 *  birth anchor and the current time. Pure + exported so the buckets are
 *  unit-tested without a clock. `null`/future anchors render `""` (no elapsed
 *  rather than a negative one); a just-launched anchor reads `0s`. */
export function formatElapsed(startedAtMs: number | null, nowMs: number): string {
  if (startedAtMs === null || !Number.isFinite(startedAtMs)) return "";
  const secs = Math.floor((nowMs - startedAtMs) / 1000);
  if (secs < 0) return "";
  return formatDurationSecs(secs);
}

/** SW-GW-8 (AC-5): the row's launcher PROVENANCE — which surface opened this
 *  agent. `"codesense"` (a nav/manual open) → "CodeSense"; a dashboard `docPath`
 *  → its short last segment (opaque, never parsed for meaning — same rule as
 *  {@link shortHandle}); `null`/empty (origin-less background) → no label. Pure +
 *  exported so the mapping is unit-tested without a render. */
export function formatLauncher(launcher: string | null): string | null {
  if (!launcher) return null;
  if (launcher === CODESENSE_LAUNCHER) return "CodeSense";
  return shortHandle(launcher);
}
