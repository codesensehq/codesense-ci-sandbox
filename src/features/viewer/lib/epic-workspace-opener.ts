// SPDX-License-Identifier: MIT
import { toast } from "sonner";

import {
  ensureEpicWorkspace,
  assembleEpicWorktrees,
  type EnsureEpicWorkspaceParams,
} from "@/tauri/commands";
import { getCurrentUser } from "@/shared/lib/get-current-user";
import { getCodesenseHome } from "@/shared/lib/codesense-home";
import { teamContentPathFromEpicDocPath } from "@/shared/lib/team-content-path";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useSurfaceSyncStore } from "@/features/shell/stores/surface-sync-store";
import { useEpicAssemblyStore } from "@/features/shell/stores/epic-assembly-store";
import { syncEpicChipEcho } from "@/features/shell/hooks/use-epic-assembly-listener";

import { openEpicTerminal, type OpenEpicTerminalDeps } from "./open-epic-terminal";

/**
 * SW-DX-15 — the epic-open orchestration, lifted out of `team-workspace-view`'s
 * `handleOpenEpicTerminal` closure into ONE shared, callable unit that BOTH the
 * nav handler AND the dashboard `openWorkspace` verb invoke. This is the `epic`
 * arm of the generic `openWorkspace({target, headed})` primitive.
 *
 * It is a **pure module** (no React): every store op reads through `getState()`,
 * and the org/team context is read imperatively via `getCurrentUser()` (the
 * sanctioned snapshot read for event handlers — `get-current-user.ts:46`) +
 * `useOrgStore.getState()`. So a non-React caller (the dashboard-launch
 * controller) can call `openEpicWorkspaceFor(target)` directly — no hook, no
 * dependency injection. Both callers converge here → the same
 * `openOrFocusEpicWorkspace` path → the shipped cap-free session register
 * (`viewer-store.ts` SW-DX-11 §2), which is what makes `sendToSession` reach the
 * live agent (AC-2).
 *
 * The resolver is parameterized by an explicit `{orgSlug, team}` (NOT the active
 * `teamKey`) so SW-DX-16 can widen it to open an arbitrary epic with zero refactor.
 */

/** The org/team/home context every epic command shares (== `EnsureEpicWorkspaceParams` minus `epicFilePath`). */
export interface EpicCommandContext {
  codesenseHome: string;
  githubUserId: number;
  orgSlug: string;
  team: string;
}

/**
 * Explicit epic-open target. `teamKey` keys the viewer-store workspace record;
 * `orgSlug`/`team` drive the command context (kept separate from `teamKey` so
 * SW-DX-16 can target another team's epic). `epicDirName`/`epicFilePath` identify
 * the epic (`epicFilePath` is the unique re-entrancy key + integration id).
 */
export interface EpicOpenTarget {
  teamKey: string;
  orgSlug: string;
  team: string;
  epicDirName: string;
  epicFilePath: string;
}

/**
 * Shared re-entrancy guard, module-level so a nav open and a dashboard open of
 * the SAME epic (keyed by `epicFilePath`) can't race two assemblies. Replaces the
 * per-component `useRef` guard the nav handler used — one epic, one in-flight
 * entry, regardless of which surface fired the open.
 */
const epicOpenInFlight = new Set<string>();

/**
 * Resolve the shared org/team/home context from an explicit `{orgSlug, team}`
 * scope. Snapshot read (safe in event handlers / non-React callers). Returns
 * `null` when the org/user isn't fully resolved — the caller decides whether to
 * toast. Mirrors the former `deriveEpicCommandContext`, but parameterized by the
 * target scope rather than the active `teamKey` (the SW-DX-16 widening hinge).
 */
export function resolveEpicCommandContext(scope: {
  orgSlug: string;
  team: string;
}): EpicCommandContext | null {
  const githubUserId = getCurrentUser()?.github_user_id ?? null;
  const org = useOrgStore.getState().orgs.find((o) => o.slug === scope.orgSlug);
  const codesenseHome = org ? getCodesenseHome() : null;
  if (!scope.team || scope.team === "_none" || githubUserId === null || !codesenseHome) {
    return null;
  }
  return { codesenseHome, githubUserId, orgSlug: scope.orgSlug, team: scope.team };
}

/**
 * SW-SR-9: derive `{orgSlug, team}` from an epic doc path for a non-React caller
 * (the `mcp:epic-repos-changed` listener) that only has the path — an agent's
 * edit can arrive with no epic view focused, so `teamKey` isn't available.
 *
 * Reuses the canonical, drift-guarded parser `teamContentPathFromEpicDocPath`
 * (anchors on `LAYER_MARKER_DIRS`, mirrors the Rust path math) for `{orgRoot,
 * team}`, then resolves `orgSlug` STRUCTURALLY by matching `orgRoot` against the
 * org store — not by positional string parsing. Returns `null` when the path is
 * non-canonical or its org isn't known; the caller then no-ops.
 */
export function deriveEpicScopeFromPath(
  epicFilePath: string,
): { orgSlug: string; team: string } | null {
  const parsed = teamContentPathFromEpicDocPath(epicFilePath);
  if (!parsed) return null;
  // Normalize trailing slashes on both sides — a divergence between how the
  // store persists `org_root` and the emitted path would otherwise silently
  // disable reassembly (write lands, but no match → no assemble).
  const stripTrailing = (p: string): string => p.replace(/\/+$/, "");
  const target = stripTrailing(parsed.orgRoot);
  const org = useOrgStore.getState().orgs.find((o) => stripTrailing(o.org_root) === target);
  if (!org) return null;
  return { orgSlug: org.slug, team: parsed.team };
}

/**
 * Build the 15-callback `OpenEpicTerminalDeps` for `target`, lifted verbatim from
 * `handleOpenEpicTerminal` (team-workspace-view.tsx). Every callback reads the
 * live store via `getState()`, so this is pure. The integration id is keyed on
 * the epic DOC PATH (SW-EW-15), matching the store, so two same-leaf epics never
 * collide.
 */
export function wireEpicTerminalDeps(target: EpicOpenTarget): OpenEpicTerminalDeps {
  const { teamKey, epicDirName, epicFilePath } = target;
  const integrationId = `integration:${epicFilePath}`;
  const hasIntegration = () =>
    (useViewerStore.getState().workspaces[teamKey] ?? []).some((w) => w.id === integrationId);
  return {
    ensureEpicWorkspace,
    assembleEpicWorktrees,
    inFlight: epicOpenInFlight,
    isAlreadyOpen: hasIntegration,
    focusExisting: () => useViewerStore.getState().switchWorkspace(teamKey, integrationId),
    // SLW-FX-27 (Part B): arm the born-"creating" placeholder synchronously.
    armEpicOpen: () =>
      useViewerStore
        .getState()
        .openOrFocusEpicWorkspace(teamKey, epicDirName, undefined, undefined, epicFilePath),
    cancelEpicOpen: () => useViewerStore.getState().closeWorkspace(teamKey, integrationId),
    isPlaceholderPresent: hasIntegration,
    // Reconcile the placeholder to the ready row once the dir resolves. Threads
    // `epicDocPath` so repo read/write resolves from the record; this call is
    // where the cap-free `dashboardRegisterSession` fires (SW-DX-11 §2) → AC-2.
    openIntegration: (dirPath, docPath) =>
      useViewerStore
        .getState()
        .openOrFocusEpicWorkspace(teamKey, epicDirName, undefined, dirPath, docPath),
    onAssemblyFailed: (workspaceDir, message) =>
      useSurfaceSyncStore.getState().markAssemblyFailure(workspaceDir, message),
    onAssemblyResolved: (workspaceDir) =>
      useSurfaceSyncStore.getState().clearAssemblyFailure(workspaceDir),
    onOpenError: (message) => toast.error(message),
    beginEpicAssembly: (workspaceDir) =>
      useEpicAssemblyStore.getState().beginEpicAssembly(workspaceDir),
    seedEpicAssembly: (workspaceDir, result) => {
      useEpicAssemblyStore.getState().seedEpicAssembly(
        workspaceDir,
        result.devEnvRuns,
        result.failures.map((f) => ({ owner: f.owner, repo: f.repo, error: f.error })),
      );
      // Reconcile the chip in case completions raced ahead of the seed.
      syncEpicChipEcho(workspaceDir);
    },
    markEpicAssemblyError: (workspaceDir, message) =>
      useEpicAssemblyStore.getState().markEpicAssemblyError(workspaceDir, message),
  };
}

/**
 * Open (or focus) an epic's integration workspace — the `epic` arm of
 * `openWorkspace`. Resolves the command context, wires the deps, and runs the
 * shared `openEpicTerminal` orchestration (materialize → assemble → spawn). On a
 * missing org/team context it toasts and no-ops (the same honest degrade the nav
 * handler had). Behavior-parity with the former `handleOpenEpicTerminal` by
 * construction: same context, same deps, same orchestration.
 */
export async function openEpicWorkspaceFor(target: EpicOpenTarget): Promise<void> {
  const ctx = resolveEpicCommandContext({ orgSlug: target.orgSlug, team: target.team });
  if (!ctx) {
    toast.error("Couldn't open epic terminal: missing org/team context.");
    return;
  }
  const base: EnsureEpicWorkspaceParams = { ...ctx, epicFilePath: target.epicFilePath };
  await openEpicTerminal(base, wireEpicTerminalDeps(target));
}
