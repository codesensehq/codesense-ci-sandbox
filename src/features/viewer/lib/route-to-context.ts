// SPDX-License-Identifier: MIT
import type { PublishFile } from "@/features/git-sync/lib/types";

/**
 * SLW-WD-4 — the app-side route/un-route contract + the PURE outcome→row-state
 * mapping the source-diff overlay renders. The Tauri commands themselves live in
 * `@/tauri/commands` (`routeFileToContext` / `unrouteFileFromContext`); this
 * module is types + pure helpers only (no I/O, no React), so the routable gate
 * and the outcome mapping are unit-testable in isolation.
 */

/**
 * The discriminated result of `routeFileToContext` — mirrors the Rust
 * `RouteOutcomeDto` (`commands/route_to_context.rs`) byte-for-byte.
 */
export type RouteOutcome =
  | { outcome: "routed" }
  | { outcome: "alreadyRouted" }
  | { outcome: "refused" }
  | { outcome: "alreadyTracked" }
  | { outcome: "notProjectable" }
  | { outcome: "collision"; sourceHash: string; destHash: string };

/**
 * Per-row optimistic route state the overlay renders (AC-3/AC-5/AC-6). `routing`
 * is the optimistic in-flight marker; the rest are terminal reconciliations of a
 * `RouteOutcome` (or a thrown command error).
 */
export type RouteState =
  | { kind: "routing" }
  | { kind: "routed" }
  | { kind: "collision" }
  | { kind: "alreadyTracked" }
  | { kind: "blocked"; reason: string }
  | { kind: "error"; message: string };

/**
 * AC-1 — a row is routable iff it is a brand-new (added), non-renamed file. WD-1
 * classifies new/untracked working-tree files as `added`; WD-3 only routes those
 * (RD-7). A tracked-but-`added` file (staged/committed) can't be told apart in
 * the frontend cheaply — it's offered here and reconciled to the `alreadyTracked`
 * hint by the backend outcome (AC-1/AC-5), keeping the list render synchronous.
 */
export function isRoutable(file: PublishFile): boolean {
  return file.status === "added" && file.renamedFrom == null;
}

/**
 * AC-5 — map a backend `RouteOutcome` to the row state, verbatim, with no guard
 * logic of WD-4's own (collision/refusal semantics live behind WD-3). Pure and
 * exhaustive over the discriminator.
 */
export function routeStateFromOutcome(outcome: RouteOutcome): RouteState {
  switch (outcome.outcome) {
    case "routed":
    case "alreadyRouted":
      return { kind: "routed" };
    case "collision":
      return { kind: "collision" };
    case "alreadyTracked":
      return { kind: "alreadyTracked" };
    case "refused":
      return {
        kind: "blocked",
        reason: "This file can’t be routed (it’s ignored, a symlink, or a secret).",
      };
    case "notProjectable":
      return {
        kind: "blocked",
        reason: "This file can’t become team context (it’s a reserved, secret, or ignored name).",
      };
  }
}
