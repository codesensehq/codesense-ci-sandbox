// SPDX-License-Identifier: MIT
import { CONTEXT_ATTACHMENTS_REFRESH } from "../lib/context-attachment-events";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { toast } from "sonner";

import { discoverLocalOrgs } from "@/shared/api/org";
import { ensureCodesenseDocs, ensureSampleRepo, type CodesenseDocsResult } from "@/tauri/commands";
import { pickLaunchOrg } from "@/shared/lib/codesense-docs";
import {
  afterPersistedSelectionRestored,
  openCodesenseDocsReadme,
} from "../lib/codesense-docs-landing";
import { loadOrg } from "@/shared/api/registry";
import { useOrgStore } from "@/shared/stores/org-store";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { logger } from "@/shared/lib/logger";
import { promiseWithTimeout } from "@/shared/lib/promise-with-timeout";
import type { Org } from "@/shared/types/domain";

// SW-FX-75: org discovery gates the loading splash (App.tsx:981). Its `finally`
// fails open on an *exception* but NOT on a *hang* — a backend call that never
// settles would strand `discoveryComplete` false and wedge the splash forever
// (the production boot hang). Bound each await so a slow/hung call degrades to
// a skip instead. Generous bounds: these calls normally take milliseconds, so
// a timeout only ever fires on a genuine wedge, never on a healthy slow boot.
const ORG_DISCOVERY_TIMEOUT_MS = 8_000;
const ORG_LOAD_TIMEOUT_MS = 6_000;
/** Placing the bundled docs repo is a local extract + checkout (~100 KB); bounded so a
 *  wedged call degrades to "no docs repo" rather than a stuck splash. */
const DOCS_PLACEMENT_TIMEOUT_MS = 15_000;
const OPEN_FILES_TOAST_ID = "org-discovery-open-files";

export function useOrgDiscovery() {
  const currentUser = useCurrentUser();
  const githubUserId = currentUser?.github_user_id ?? null;
  const principal = currentUser?.principal ?? null;
  const setOrgs = useOrgStore((s) => s.setOrgs);
  const setActiveOrg = useOrgStore((s) => s.setActiveOrg);
  const discoveryComplete = useOrgStore((s) => s.discoveryComplete);
  const setDiscoveryComplete = useOrgStore((s) => s.setDiscoveryComplete);
  const beginDiscovery = useOrgStore((s) => s.beginDiscovery);

  const [revision, setRevision] = useState(0);
  useEffect(() => {
    let live = true;
    const refresh = () => setRevision((value) => value + 1);
    window.addEventListener(CONTEXT_ATTACHMENTS_REFRESH, refresh);
    const subscription = listen("context-attachments-changed", async () => {
      try {
        if (live) setRevision((value) => value + 1);
      } catch (error) {
        logger.warn("org", `Attachment refresh failed: ${String(error)}`);
      }
    }).catch((error: unknown) => {
      logger.warn("org", `Attachment subscription failed: ${String(error)}`);
      return () => {};
    });
    return () => {
      live = false;
      window.removeEventListener(CONTEXT_ATTACHMENTS_REFRESH, refresh);
      void subscription.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    if (githubUserId === null) return;

    let cancelled = false;
    let stopDeciding = () => {};
    let transientFailure = false;

    function reportTransient(error: unknown) {
      const message = String(error);
      if (!message.toLowerCase().includes("too many open files")) return;
      transientFailure = true;
      toast.error(`Context repos could not load: ${message}`, {
        id: OPEN_FILES_TOAST_ID,
        duration: Infinity,
        action: { label: "Retry", onClick: () => setRevision((value) => value + 1) },
      });
    }

    async function discover() {
      beginDiscovery();
      try {
        logger.info("boot", "org discovery: start");
        // SW-OC-16: every install has the CodeSense docs context repo. Place and connect
        // it before discovery so it is always one of the context repos found.
        let docs: CodesenseDocsResult | null = null;
        try {
          docs = await promiseWithTimeout(ensureCodesenseDocs(), DOCS_PLACEMENT_TIMEOUT_MS, () => {
            logger.warn("boot", "org discovery: CodeSense docs placement watchdog fired");
            return null;
          });
        } catch (e) {
          logger.warn("boot", `org discovery: CodeSense docs unavailable — ${String(e)}`);
        }
        // SW-ODC-11: place the sample source repo alongside the docs. Non-fatal; the
        // dashboards fall back to seed-data-only mode when this returns `refused` or
        // when the call errors out.
        try {
          await promiseWithTimeout(ensureSampleRepo(), DOCS_PLACEMENT_TIMEOUT_MS, () => {
            logger.warn("boot", "org discovery: sample source repo placement watchdog fired");
            return null;
          });
        } catch (e) {
          logger.warn("boot", `org discovery: sample source repo unavailable — ${String(e)}`);
        }
        if (cancelled) return;
        const summaries = await promiseWithTimeout(
          discoverLocalOrgs(githubUserId!),
          ORG_DISCOVERY_TIMEOUT_MS,
          () => {
            logger.warn(
              "boot",
              `org discovery: discoverLocalOrgs watchdog fired after ${ORG_DISCOVERY_TIMEOUT_MS}ms — proceeding with no orgs`,
            );
            return [];
          },
        );

        if (cancelled) return;

        if (summaries.length === 0) {
          toast.dismiss(OPEN_FILES_TOAST_ID);
          setOrgs([]);
          setDiscoveryComplete(true);
          return;
        }

        const loadedOrgs: Org[] = [];
        for (const summary of summaries) {
          if (summary.availability_error) continue;
          try {
            const loaded = await promiseWithTimeout(
              loadOrg(summary.slug, summary.org_root, summary.remote_url, principal),
              ORG_LOAD_TIMEOUT_MS,
              () => {
                // Throw → skipped by the catch below. A single slow/hung org
                // must not strand discovery (which gates the splash).
                throw new Error(`loadOrg watchdog fired after ${ORG_LOAD_TIMEOUT_MS}ms`);
              },
            );
            const transient = loaded.file_errors?.find(
              (error) => error.kind === "TransientReadError",
            );
            if (transient) throw new Error(transient.message);
            if (loaded.org) {
              loadedOrgs.push({ ...loaded.org, is_attached: summary.is_attached === true });
            }
          } catch (e) {
            logger.warn("boot", `org discovery: loadOrg skipped for ${summary.slug} — ${e}`);
            reportTransient(e);
            // Preserve a previously loaded org while its files are temporarily unavailable.
            if (String(e).toLowerCase().includes("too many open files")) {
              const known = useOrgStore.getState().orgs.find((org) => org.slug === summary.slug);
              if (known) loadedOrgs.push(known);
            }
          }
        }

        if (cancelled) return;

        if (!transientFailure) toast.dismiss(OPEN_FILES_TOAST_ID);
        const previous = useOrgStore.getState();
        const newlyAttached =
          revision > 0
            ? loadedOrgs.find(
                (org) => org.is_attached && !previous.orgs.some((old) => old.slug === org.slug),
              )
            : undefined;
        const lastOpenedBeforeRefresh = previous.activeOrgSlug;
        setOrgs(loadedOrgs);
        // Later launches reopen the last opened context repo (and, through the per-repo
        // memory, its last team). Only the very first launch, proven by a restored session
        // with no last-opened repo, lands in the CodeSense docs with its README open.
        stopDeciding = afterPersistedSelectionRestored(() => {
          if (cancelled) return;
          // The last-opened repo, whether the restore landed before `setOrgs` (which drops
          // a selection that is no longer connected) or after it.
          const restored = useOrgStore.getState().activeOrgSlug ?? lastOpenedBeforeRefresh;
          const launch = pickLaunchOrg({
            orgs: loadedOrgs,
            activeOrgSlug: restored,
            newlyConnected: newlyAttached?.slug,
          });
          if (launch.slug && launch.slug !== useOrgStore.getState().activeOrgSlug) {
            setActiveOrg(launch.slug);
          }
          if (launch.firstLaunch && docs) openCodesenseDocsReadme(docs.readme_path);
        });
      } catch (error) {
        reportTransient(error);
        logger.warn("boot", `org discovery failed: ${String(error)}`);
      } finally {
        if (!cancelled) {
          logger.info("boot", "org discovery: complete");
          setDiscoveryComplete(true);
        }
      }
    }

    discover();

    return () => {
      cancelled = true;
      stopDeciding();
    };
  }, [
    githubUserId,
    principal,
    revision,
    setOrgs,
    setActiveOrg,
    setDiscoveryComplete,
    beginDiscovery,
  ]);

  // SW-FU-B3: with no current user there is nothing to discover. Report the
  // gate as complete so App can route to Login instead of holding the splash
  // forever; discovery runs once a user appears.
  return { discoveryComplete: discoveryComplete || githubUserId === null };
}
