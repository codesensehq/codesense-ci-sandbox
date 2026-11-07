// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";

import { discoverLocalOrgs } from "@/shared/api/org";
import { loadOrg } from "@/shared/api/registry";
import { rendererPrincipal } from "@/shared/lib/local-profile-session";
import { getCurrentUser } from "@/shared/lib/get-current-user";
import { logger } from "@/shared/lib/logger";
import { promiseWithTimeout } from "@/shared/lib/promise-with-timeout";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org } from "@/shared/types/domain";
import {
  ACTIVE_TEAM_SWITCH_EVENT,
  ORG_REGISTRY_CHANGED_EVENT,
  type ActiveTeamSwitchPayload,
  type OrgRegistryChangedPayload,
} from "@/tauri/events";

/** Bound on one org read, matching boot discovery: a hung load must not wedge
 *  the org's queue (every later change to that org waits behind it). */
export const ORG_RELOAD_TIMEOUT_MS = 6_000;

export const SOURCE_ATTACHMENTS_CHANGED_EVENT = "source-attachments-changed";

export async function applySourceAttachmentChange(change: { principal: string }): Promise<void> {
  if (change.principal !== (await rendererPrincipal())) return;
  useOrgStore.getState().bumpAttachmentsVersion();
}

/**
 * Per-org queue: each org's registry changes and team switches apply in the
 * order Rust emitted them. A rename must move team-keyed state before a later
 * reload drops the old team name, and a switch to a just-created team must wait
 * for the reload that lists it.
 */
const pendingByOrg = new Map<string, Promise<void>>();

/**
 * Team switches are ordered across orgs too: only the latest switch may land.
 * A switch queued behind its org's reload must not override a later switch to
 * another org that applied first.
 */
let switchGeneration = 0;

function enqueue(orgSlug: string, work: () => Promise<void>): Promise<void> {
  const next = (pendingByOrg.get(orgSlug) ?? Promise.resolve()).then(work).catch((e: unknown) => {
    logger.warn("org-registry", `${orgSlug}: ${e instanceof Error ? e.message : String(e)}`);
  });
  pendingByOrg.set(orgSlug, next);
  void next.finally(() => {
    if (pendingByOrg.get(orgSlug) === next) pendingByOrg.delete(orgSlug);
  });
  return next;
}

async function loadOrgBounded(
  slug: string,
  orgRoot: string,
  remoteUrl: string | null,
): Promise<Org | null> {
  const loaded = await promiseWithTimeout(
    loadOrg(slug, orgRoot, remoteUrl, getCurrentUser()?.principal ?? null),
    ORG_RELOAD_TIMEOUT_MS,
    () => {
      throw new Error(`loading ${slug} timed out after ${ORG_RELOAD_TIMEOUT_MS}ms`);
    },
  );
  const transient = loaded.file_errors?.find((error) => error.kind === "TransientReadError");
  if (transient) {
    logger.warn("org-registry", `${slug}: ${transient.message}`);
    return null;
  }
  if (!loaded.org) {
    logger.warn("org-registry", `${slug}: the org's registry does not load`);
    return null;
  }
  return loaded.org;
}

/** Re-read an org the store lists from disk; `null` when the store doesn't
 *  list it or it no longer composes (its registry files carry errors — the next
 *  good edit reloads it again). */
async function reloadOrg(orgSlug: string): Promise<Org | null> {
  const known = useOrgStore.getState().orgs.find((o) => o.slug === orgSlug);
  if (!known) return null;
  return loadOrgBounded(known.slug, known.org_root, known.remote_url ?? null);
}

/** Find a local org the store doesn't list yet (e.g. one cloned since boot)
 *  through the same discovery the boot path uses, and load it. */
async function discoverOrg(orgSlug: string): Promise<Org | null> {
  const user = getCurrentUser();
  if (!user) return null;
  const summaries = await promiseWithTimeout(
    discoverLocalOrgs(user.github_user_id),
    ORG_RELOAD_TIMEOUT_MS,
    () => {
      throw new Error(`discovering orgs timed out after ${ORG_RELOAD_TIMEOUT_MS}ms`);
    },
  );
  const summary = summaries.find((s) => s.slug === orgSlug);
  if (!summary) return null;
  return loadOrgBounded(summary.slug, summary.org_root, summary.remote_url);
}

/** Put `org` into the store, replacing the copy it holds or adding it. */
function storeOrg(org: Org): void {
  const orgs = useOrgStore.getState().orgs;
  const next = orgs.some((o) => o.slug === org.slug)
    ? orgs.map((o) => (o.slug === org.slug ? org : o))
    : [...orgs, org];
  useOrgStore.getState().setOrgs(next);
}

/** The backend moved to a team the UI can't show: say so, rather than let the
 *  UI quietly stay elsewhere (its next team activation would move the backend
 *  back). */
function reportUnfollowedSwitch(switched: ActiveTeamSwitchPayload): void {
  const message = `An agent switched to ${switched.orgSlug} · ${switched.team}, but this window can't show that team.`;
  logger.warn("org-registry", message);
  toast.warning(message);
}

/** Reload the changed org and reconcile it into the store. */
export function applyOrgRegistryChange(change: OrgRegistryChangedPayload): Promise<void> {
  return enqueue(change.orgSlug, async () => {
    // The event may finish after native profile selection; never interpret
    // another principal's rename/delete against this renderer's workspace.
    if (change.principal !== (await rendererPrincipal())) return;
    const org = await reloadOrg(change.orgSlug);
    await useOrgStore.getState().applyOrgRegistryChange(org, change);
  });
}

/** Show the team an agent switched to, loading the org first when the store
 *  doesn't list the team (or the org) yet. A switch overtaken by a later one is
 *  dropped. */
export function applyActiveTeamSwitch(switched: ActiveTeamSwitchPayload): Promise<void> {
  const generation = ++switchGeneration;
  return enqueue(switched.orgSlug, async () => {
    const store = useOrgStore.getState();
    const knownOrg = store.orgs.find((o) => o.slug === switched.orgSlug);
    if (!knownOrg?.teams.some((t) => t.name === switched.team)) {
      const org = knownOrg
        ? await reloadOrg(switched.orgSlug)
        : await discoverOrg(switched.orgSlug);
      if (generation !== switchGeneration) return;
      if (!org) {
        reportUnfollowedSwitch(switched);
        return;
      }
      storeOrg(org);
    }
    if (generation !== switchGeneration) return;
    if (!useOrgStore.getState().switchToTeam(switched.orgSlug, switched.team)) {
      reportUnfollowedSwitch(switched);
    }
  });
}

/**
 * App-root bridge that keeps the org store in step with registry edits and team
 * switches made outside the UI — chiefly an agent setting CodeSense up over
 * MCP. Rust emits {@link ORG_REGISTRY_CHANGED_EVENT} after every registry edit
 * (the UI's own included; applying those again is a no-op),
 * {@link ACTIVE_TEAM_SWITCH_EVENT} after an MCP team switch, and
 * {@link SOURCE_REPO_ATTACHMENTS_CHANGED_EVENT} after a local clone is bound or
 * unbound (re-reading the attachments again is harmless).
 */
export function useOrgRegistrySync(): void {
  useEffect(() => {
    const group = createListenerGroup();
    group.track(
      listen<{ principal: string }>(SOURCE_ATTACHMENTS_CHANGED_EVENT, ({ payload }) => {
        if (payload?.principal) void applySourceAttachmentChange(payload);
      }),
    );
    group.track(
      listen<OrgRegistryChangedPayload>(ORG_REGISTRY_CHANGED_EVENT, ({ payload }) => {
        if (payload?.orgSlug) void applyOrgRegistryChange(payload);
      }),
    );
    group.track(
      listen<ActiveTeamSwitchPayload>(ACTIVE_TEAM_SWITCH_EVENT, ({ payload }) => {
        if (payload?.orgSlug && payload.team) void applyActiveTeamSwitch(payload);
      }),
    );
    return () => group.dispose();
  }, []);
}
