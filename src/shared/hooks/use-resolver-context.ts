// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import { getCodesenseHome } from "@/shared/lib/codesense-home";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { useActiveOrg } from "@/shared/stores/use-active-org";
import { useOrgStore } from "@/shared/stores/org-store";

/**
 * Resolver context passed into every content-engine Tauri command that
 * needs `(orgSlug, teamName, codesenseHome, userId)` — the install
 * workbench (SS-13) and the shared canvas's wireframe libraries (SW-SC-5).
 * Mirrors the engine's `ResolverContext` shape, but exposed exclusively
 * through this hook so consumers never assemble it ad-hoc.
 *
 * Lives in `shared/` because both the workbench feature and the dashboard
 * canvas depend on it — promoted here (from the workbench feature) in
 * SW-SC-5; a re-export shim keeps the old workbench import path working.
 */
export interface ResolverContext {
  orgSlug: string;
  teamName: string;
  codesenseHome: string;
  userId: number;
}

/**
 * SS-FU-24 (AC-5): the misconfigured variant — an active org/team/user exists
 * AND `org_root` is populated, but the CodeSense home could not be read. Distinct from a bare `null` (which means "no team selected") so the
 * workbench can render an explicit "something is misconfigured" state instead
 * of a silently-empty, indistinguishable empty view.
 */
export interface ResolverContextError {
  error: "unresolved-home";
}

/** Discriminated result of {@link useResolverContext}:
 *  - `ResolverContext` — ready to use.
 *  - `null` — no org/team/user selected, or `org_root` not yet populated
 *    (a transient mount-timing race that resolves on the next render).
 *  - `ResolverContextError` — org root present but unparseable (loud failure). */
export type ResolverContextResult = ResolverContext | null | ResolverContextError;

/** Narrow a {@link ResolverContextResult} to the ready `ResolverContext`.
 *  Consumers that only act on a usable context use this to collapse both the
 *  `null` and error variants to "not ready". */
export function isResolverContext(result: ResolverContextResult): result is ResolverContext {
  return result !== null && !("error" in result);
}

/**
 * Composes `(orgSlug, teamName, codesenseHome, userId)` from existing sources:
 *
 * - `orgSlug` / `teamName` from the active org (`useActiveOrg`) and
 *   `useOrgStore` (workspace state).
 * - `userId` from `useCurrentUser()` (auth state).
 * - `codesenseHome` from `getCodesenseHome()` — `~/.codesense`, resolved once
 *   before first render (the same home Rust uses), never parsed from a path.
 *
 * Returns `null` when any of `(activeOrg, teamName, userId)` is missing, or
 * when `org_root` is not yet populated (the modal renders its AC-15 "no team
 * selected" empty state). Returns `{ error: "unresolved-home" }` when
 * the CodeSense home could not be read, so the caller can surface an explicit
 * error state (AC-5) rather than masking it as an empty layer.
 */
export function useResolverContext(): ResolverContextResult {
  const teamName = useOrgStore((s) => s.activeTeamName);
  const org = useActiveOrg();
  const user = useCurrentUser();

  return useMemo(() => {
    if (!org || !teamName || !user) return null;

    // org_root not yet populated at mount — a transient race, not a
    // misconfiguration. Fall back to the "no team" empty state; the next
    // render (once the org loads) resolves it.
    if (!org.org_root) return null;

    // The one app-wide home, resolved before first render. Null only if the
    // OS home could not be read — surfaced as the error variant.
    const codesenseHome = getCodesenseHome();
    if (codesenseHome === null) {
      return { error: "unresolved-home" };
    }

    return {
      orgSlug: org.slug,
      teamName,
      codesenseHome,
      userId: user.github_user_id,
    };
  }, [org, teamName, user]);
}

/**
 * Convenience wrapper for consumers that only act on a *ready* context and
 * defer the no-team / misconfigured empty states to the modal's router.
 * Collapses both the `null` (no team) and `{ error }` (unresolved-home)
 * variants to `null`, so call sites avoid re-deriving the `isResolverContext`
 * narrow. Use this in workbench components/hooks; use {@link useResolverContext}
 * directly only where the discriminated variant drives rendering (the modal).
 */
export function useReadyResolverContext(): ResolverContext | null {
  const result = useResolverContext();
  return isResolverContext(result) ? result : null;
}
