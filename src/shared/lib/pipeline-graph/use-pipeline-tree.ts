// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  CONTENT_SCAN_TIMEOUT_MS,
  ContentScanSuperseded,
  requestContentScan,
} from "./content-scan-queue";
import { useWindowVisible } from "@/shared/hooks/use-window-focused";
import { useLoadingDeadline } from "@/shared/hooks/use-loading-deadline";
import { useContentSource } from "@/shared/hooks/use-content-source";
import type { ContentSource } from "@/shared/lib/team-content-context";
import { parseContextId } from "@/shared/lib/context-model";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useSummaryHeadings } from "@/shared/hooks/use-summary-headings";
import { parseSyntheticRepoKey } from "@/shared/lib/build-synthetic-repo";
import { logger } from "@/shared/lib/logger";
import { createWarmCache } from "@/shared/lib/warm-scopes";
import type { ScopeKey } from "@/shared/lib/warm-scopes";
import type { DerivedPipelineTree } from "./types";
import { usePipelineHydrated } from "./use-pipeline-hydrated";

// Serialize only the structure-relevant fields of the tree so a pure
// content edit (story H1 title tweak) produces the same fingerprint as
// the previous scan. Title text is dropped; everything else — dirPath,
// filenames, orderError, absoluteDirPath, mainDoc, depth — stays.
// Used by usePipelineTree below to decide whether a rescan's result is
// structurally identical to the previous one.
function structuralFingerprint(tree: DerivedPipelineTree): string {
  return JSON.stringify(tree, (key, value) => (key === "title" ? undefined : value));
}

export type UsePipelineTreeResult = {
  tree: DerivedPipelineTree | null;
  loading: boolean;
  error: string | null;
  refetch: () => void;
};

const SCAN_DEBOUNCE_MS = 150;

const SCOPE_SEP = "\u0000";
// The last committed tree per team and scan input, kept while the team is at least suspended: a
// team whose view was unmounted (suspended, or evicted from the warm set) comes back showing its
// last tree and revalidates behind it, instead of a "Loading team context…" frame.
const treeCache = createWarmCache<string, DerivedPipelineTree>({
  kind: "pipeline-tree",
  scopeOf: (key) => key.slice(0, key.indexOf(SCOPE_SEP)),
  tier: "suspended",
});

/**
 * Hook that scans the derived pipeline tree for a given repo + scope and
 * returns the result (story 30.7 AC-18).
 *
 * Behavior:
 * - Gated on `usePipelineHydrated()` — during hydration returns
 *   `{ tree: null, loading: true, error: null, refetch: noop }` and makes
 *   ZERO scanner calls.
 * - Short-circuits to `{ tree: null, loading: false, error: null }` when
 *   `teamContentPath === ""`.
 * - Reads the per-repo file-refresh key so the scanner re-runs on file
 *   watcher ticks.
 * - Trailing-debounce at 150ms to coalesce rapid triggers.
 * - Generation counter discards stale scan results.
 * - Cleanup on unmount: clears the debounce timer and bumps the generation
 *   counter (in-flight scans are discarded).
 * - `refetch()` is a stable callback that bumps an internal manual-trigger
 *   counter, causing the effect to re-run and fire a fresh scan. UI
 *   components MUST use this rather than touching fileRefreshKeys.
 */
export function usePipelineTree(
  repoPath: string,
  scopePath: string,
  // Existing source-scoped callers can provide a narrow root adapter while
  // their discovered configuration is loading. A resolved owner takes priority.
  repoOverride?: ContentSource,
  enabled = true,
  // The team this tree belongs to. Given, its tree is kept warm across the view's unmounts.
  warmScope?: ScopeKey,
): UsePipelineTreeResult {
  const hydrated = usePipelineHydrated();
  const visible = useWindowVisible();
  const [manualTrigger, setManualTrigger] = useState(0);
  const [scheduleTrigger, setScheduleTrigger] = useState(0);
  const bootstrapExpired = useLoadingDeadline(!hydrated, `${repoPath}:${manualTrigger}`);

  // Narrow selectors — the hook re-runs only when the relevant repo's
  // teamContentPath / mainDoc / refresh-key changes.
  const resolvedRepo = useContentSource(repoPath) ?? repoOverride;
  const teamContentPath = resolvedRepo?.teamContentPath ?? "";
  const mainDoc = resolvedRepo?.mainDoc ?? "";
  const refreshKey = useFileRefreshStore((s) => s.fileRefreshKeys[repoPath] ?? 0);
  // SW-EW-32: the epic-node purpose banner keys off the configured summary
  // headings. A change re-runs the scan effect (dep below); the re-scan
  // recomputes `purposeText` fresh (build_main_doc reads disk every scan —
  // not cached), and `structuralFingerprint` includes `purposeText`, so the
  // new banner commits via stale-while-revalidate (no skeleton flash).
  const summaryHeadings = useSummaryHeadings();

  // SLW-MD-1 (Task 6 / AC-11): resolve the active team's CONFIGURED source
  // repos + the org's valid team names once, and thread them into the scan so
  // it can stamp a per-doc `metadataValid` flag (source_repo configured-check
  // + team-check) without a lockfile walk. Derived from the synthetic
  // repoPath's team and the loaded org config in the store. Empty (null) /
  // empty configured set is a safe no-op: source_repo membership badging is
  // disabled (well-formed values checked syntactic-only, not flagged on
  // membership; a malformed slug is still flagged), so a transiently
  // unresolved org never badges every present source_repo.
  const orgs = useOrgStore((s) => s.orgs);
  const { configuredSourceRepos, validTeams } = useMemo(() => {
    const content = parseContextId(repoPath);
    const parsed =
      content?.kind === "team-all"
        ? { orgSlug: content.orgSlug, team: content.teamName }
        : parseSyntheticRepoKey(repoPath);
    if (!parsed) return { configuredSourceRepos: undefined, validTeams: undefined };
    const org = orgs.find((o) => o.slug === parsed.orgSlug);
    if (!org) return { configuredSourceRepos: undefined, validTeams: undefined };
    const teamNames = org.teams.map((t) => t.name);
    const team = org.teams.find((t) => t.name === parsed.team);
    const repos = team?.source_repos.map((r) => `${r.owner}/${r.repo}`);
    return { configuredSourceRepos: repos, validTeams: teamNames };
  }, [orgs, repoPath]);

  // The scan result lives in state only while a scan is running or has
  // completed. Non-scan states (not-hydrated, no-config) are derived below
  // so we don't `setState` in an effect for purely derived values.
  const inputKey = `${repoPath}|${teamContentPath}|${mainDoc}|${scopePath}`;
  const cacheKey = warmScope ? `${warmScope}${SCOPE_SEP}${inputKey}` : null;
  // A returning team starts from its cached tree; the scan below revalidates behind it.
  const [seed] = useState(() => (cacheKey ? treeCache.get(cacheKey) : undefined));
  const [scanState, setScanState] = useState<Omit<UsePipelineTreeResult, "refetch">>(
    seed ? { tree: seed, loading: false, error: null } : { tree: null, loading: true, error: null },
  );

  // Generation counter so stale scans can't overwrite newer state.
  const generationRef = useRef(0);
  const firstDirtyAtRef = useRef<number | null>(null);
  const debounceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const waitingForIdleRef = useRef(false);
  const previousManualTriggerRef = useRef(-1);
  const waitingSinceRef = useRef(0);
  const failedRef = useRef(false);
  const hasTreeRef = useRef(seed !== undefined);
  // Fingerprint of the last tree we committed to state. Used to skip
  // state updates (and the downstream Dagre re-layout) when a rescan
  // produces a structurally identical tree — e.g. the user just saved
  // a story file's contents without touching its filename or the epic's
  // pipeline.json. Without this, every file-watcher tick flashes the
  // pipeline through a loading skeleton and re-runs layout.
  const lastFingerprintRef = useRef<string | null>(seed ? structuralFingerprint(seed) : null);
  // Previous scan inputs (everything except the refresh tick). When any
  // of these change — repo selection, scope drill, viewer/pipeline root
  // edit — the fingerprint cache is no longer comparable, so we reset
  // it and let the next scan commit normally (producing the expected
  // loading skeleton while the user drills into a new area).
  const prevScanKeyRef = useRef<string>(seed ? inputKey : "");

  const refetch = useCallback(() => {
    setManualTrigger((n) => n + 1);
  }, []);

  // PTS-2 / R-14: the scanner scopes at the content root, so only
  // teamContentPath is required. SW-LN-B21: `contextRepoPath` is NOT — Rust
  // hydrates it only at org discovery, so a team's first repo attached (or the
  // sample seeded) at runtime has it "" until a reload, and requiring it left the
  // nav blank on the empty-team → first-content transition.
  const available = hydrated && !!teamContentPath && enabled && visible && !bootstrapExpired;
  const scanExpired = useLoadingDeadline(
    available && (scanState.loading || prevScanKeyRef.current !== inputKey),
    `${inputKey}:${manualTrigger}`,
  );
  const canScan = available && !scanExpired;
  useEffect(() => {
    if (canScan) return;
    generationRef.current += 1;
    waitingSinceRef.current = 0;
    waitingForIdleRef.current = false;
  }, [canScan]);
  useEffect(
    () => () => {
      generationRef.current += 1;
    },
    [],
  );

  useEffect(() => {
    if (!canScan) return;

    // Detect non-refresh input changes (repo, scope, roots) and invalidate
    // the fingerprint cache + drop any cached tree so the user sees a
    // normal loading skeleton while the new area scans.
    const scanKey = `${repoPath}|${teamContentPath}|${mainDoc}|${scopePath}`;
    const scanKeyChanged = prevScanKeyRef.current !== scanKey;
    let cached: DerivedPipelineTree | undefined;
    if (scanKeyChanged) {
      prevScanKeyRef.current = scanKey;
      firstDirtyAtRef.current = null;
      // A key that resolves after mount (a Private root) seeds from the cache like a mount does.
      cached = cacheKey ? treeCache.get(cacheKey) : undefined;
      lastFingerprintRef.current = cached ? structuralFingerprint(cached) : null;
      hasTreeRef.current = cached !== undefined;
    }
    const manualRetry = previousManualTriggerRef.current !== manualTrigger;
    // A failed reader requires deliberate recovery. Watcher bursts must not
    // resubmit the same failing job or erase the actionable failure indicator.
    if ((failedRef.current || waitingForIdleRef.current) && !scanKeyChanged && !manualRetry) return;
    waitingForIdleRef.current = false;
    failedRef.current = false;
    previousManualTriggerRef.current = manualTrigger;
    if (scanKeyChanged || manualRetry || hasTreeRef.current || !waitingSinceRef.current)
      waitingSinceRef.current = Date.now();

    // Refreshes keep valid same-owner reads publishable; only identity changes
    // and teardown invalidate them. Otherwise continuous writes starve first paint.
    if (scanKeyChanged) generationRef.current += 1;
    const generation = generationRef.current;
    if (debounceTimerRef.current !== null) {
      clearTimeout(debounceTimerRef.current);
    }
    // Stale-while-revalidate on refresh-only ticks: keep the cached tree
    // visible while the background scan runs. On a scan-key change (drill,
    // root edit, repo switch) we drop the cached tree so the user sees
    // the normal loading skeleton for the new area.
    setScanState((prev) => {
      if (scanKeyChanged) {
        return cached
          ? { tree: cached, loading: false, error: null }
          : { tree: null, loading: true, error: null };
      }
      if (prev.tree === null) {
        return prev.loading && prev.error === null ? prev : { ...prev, loading: true, error: null };
      }
      // Already have a tree — clear any stale error but keep showing it
      // while the background scan runs.
      return prev.error === null ? prev : { ...prev, error: null };
    });

    firstDirtyAtRef.current ??= Date.now();
    const wait = Math.max(
      0,
      Math.min(SCAN_DEBOUNCE_MS, 2_000 - (Date.now() - firstDirtyAtRef.current)),
    );
    debounceTimerRef.current = setTimeout(() => {
      debounceTimerRef.current = null;
      firstDirtyAtRef.current = null;
      // PTS-2 / R-14: scanner takes teamContentPath directly; the scan-scope
      // arg is hardcoded "" (scope-collapse at the scanner boundary). This ""
      // is flow-2 (the scanner IPC param), NOT the override-glob prefix.
      requestContentScan(
        [
          teamContentPath,
          "",
          scopePath,
          mainDoc,
          summaryHeadings,
          configuredSourceRepos,
          validTeams,
        ],
        `${refreshKey}:${manualTrigger}`,
        CONTENT_SCAN_TIMEOUT_MS - (Date.now() - waitingSinceRef.current),
      )
        .then((tree) => {
          if (generation !== generationRef.current) return;
          failedRef.current = false;
          hasTreeRef.current = true;
          const newFingerprint = structuralFingerprint(tree);
          if (newFingerprint === lastFingerprintRef.current) {
            // Structurally identical to the previously committed tree —
            // preserve the existing reference so pipeline-viewer-panel's
            // `useMemo([tree])` skips the Dagre re-layout. Only touch
            // loading/error flags if they actually need clearing.
            setScanState((prev) =>
              prev.loading || prev.error !== null
                ? { tree: prev.tree, loading: false, error: null }
                : prev,
            );
            return;
          }
          lastFingerprintRef.current = newFingerprint;
          if (cacheKey) treeCache.set(cacheKey, tree);
          setScanState({ tree, loading: false, error: null });
        })
        .catch((e: unknown) => {
          if (generation !== generationRef.current) return;
          if (e instanceof ContentScanSuperseded) {
            waitingForIdleRef.current = true;
            void e.whenIdle.then(() => {
              if (generation !== generationRef.current) return;
              waitingForIdleRef.current = false;
              setScheduleTrigger((value) => value + 1);
            });
            return;
          }
          failedRef.current = true;
          const message = e instanceof Error ? e.message : String(e);
          logger.warn("pipeline-scanner", `scanPipelineTree failed for ${repoPath}: ${message}`);
          lastFingerprintRef.current = null;
          setScanState((prev) => ({ tree: prev.tree, loading: false, error: message }));
        });
    }, wait);

    return () => {
      if (debounceTimerRef.current !== null) {
        clearTimeout(debounceTimerRef.current);
        debounceTimerRef.current = null;
      }
    };
  }, [
    canScan,
    scheduleTrigger,
    teamContentPath,
    mainDoc,
    scopePath,
    repoPath,
    refreshKey,
    manualTrigger,
    summaryHeadings,
    configuredSourceRepos,
    validTeams,
    cacheKey,
  ]);

  if (!hydrated || bootstrapExpired) {
    return {
      tree: null,
      loading: !bootstrapExpired,
      error: bootstrapExpired
        ? "Loading team context timed out. Retry after workspace setup finishes."
        : null,
      refetch,
    };
  }
  if (!teamContentPath) {
    return { tree: null, loading: false, error: null, refetch };
  }
  if (scanExpired)
    return {
      tree: prevScanKeyRef.current === inputKey ? scanState.tree : null,
      loading: false,
      error: "Reading team context timed out. Retry when the scan finishes.",
      refetch,
    };
  const cachedForKey = cacheKey ? treeCache.get(cacheKey) : undefined;
  return prevScanKeyRef.current !== inputKey
    ? cachedForKey
      ? { tree: cachedForKey, loading: false, error: null, refetch }
      : { tree: null, loading: true, error: null, refetch }
    : { ...scanState, refetch };
}
