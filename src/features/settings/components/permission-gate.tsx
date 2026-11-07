// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";
import type { WorkspacePermission } from "@/shared/stores/org-store";

/**
 * ADT-style requirement strings, not raw permission tiers. Semantic
 * aliases keep every call site from re-encoding the "null is full-
 * control" rule. Add new aliases here when future stories need them
 * (e.g., "admin-only-never-null" if we ever want to exclude local
 * workspaces from a destructive action).
 *
 * - `"admin"` — admin OR null (local-only workspaces always pass).
 * - `"any-writer"` — admin / write / null (anything except `"read"`).
 */
export type PermissionRequirement = "admin" | "any-writer";

interface PermissionGateProps {
  required: PermissionRequirement;
  children: ReactNode;
  /** Rendered when the gate denies access. Defaults to nothing (the
   *  affordance is simply hidden). Pass e.g. a read-only `<p>` to show
   *  users what they can't edit. */
  fallback?: ReactNode;
}

/**
 * SW-OB-7 (open access — everyone is a lead): capability is unconditional,
 * so the gate always renders its children. No remote permission is consulted
 * in the render path — this removes the settings UI's dependency on the async
 * GitHub permission fetch, so gated affordances render synchronously with no
 * network wait or flicker.
 *
 * Unwire-don't-delete: the props and the `permissionSatisfies` rule table are
 * retained (inert) so the future re-add of remote-backed gating is one pass. The remote permission *fetch* itself is
 * removed in SW-OB-8.
 */
export function PermissionGate({ children }: PermissionGateProps) {
  return <>{children}</>;
}

/**
 * The rule table in one place. Exported for unit tests and for rare
 * call sites that need to mirror a gate decision in a non-render path
 * (e.g., a keyboard shortcut handler). Prefer `<PermissionGate>` for
 * anything that renders.
 */
export function permissionSatisfies(
  permission: WorkspacePermission,
  required: PermissionRequirement,
): boolean {
  if (permission === null) return true;
  if (required === "admin") return permission === "admin";
  return permission !== "read";
}
