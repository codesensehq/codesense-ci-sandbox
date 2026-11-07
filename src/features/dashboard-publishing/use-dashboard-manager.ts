// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import * as api from "@/tauri/commands";
import { CONTEXT_INDEX_CHANGED_EVENT, type ContextIndexChangedPayload } from "@/tauri/events";
import type { DashboardDetail, DashboardInventory, InventoryRow } from "./types";
import { dashboardRowKey } from "./dashboard-groups";

const dashboardManifestPath = /(?:^|\/)content\/dashboards\/[^/]+\/dashboard\.yml$/;

type DashboardSelection = { key: string; generation: number };

export function useDashboardManager() {
  const [key, setKey] = useState<api.DashboardPublisherKeyStatus | null>(null);
  const keyHint = useRef<string | null>(null);
  const [inventory, setInventory] = useState<DashboardInventory | null>(null);
  const inventoryRef = useRef<DashboardInventory | null>(null);
  const [selected, setSelected] = useState<DashboardSelection | null>(null);
  const selectionRef = useRef<DashboardSelection | null>(null);
  const selectionGeneration = useRef(0);
  const detailGeneration = useRef(0);
  const [detail, setDetail] = useState<DashboardDetail | null>(null);
  const [detailError, setDetailError] = useState("");
  const [busy, setBusy] = useState<string | null>("Loading…");
  const working = useRef(false);
  const inventoryGeneration = useRef(0);
  const localInventoryGeneration = useRef(0);
  const keyStatusGeneration = useRef(0);
  const pendingLocalRefresh = useRef(false);
  const refreshLocalRef = useRef<() => void>(() => {});
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const firstLoad = useRef(true);

  const updateInventory = useCallback((next: DashboardInventory | null) => {
    inventoryRef.current = next;
    setInventory(next);
  }, []);
  const updateSelection = useCallback((key: string | null) => {
    const next = key ? { key, generation: ++selectionGeneration.current } : null;
    selectionRef.current = next;
    setSelected(next);
    setDetail(null);
    setDetailError("");
    return next;
  }, []);
  const isCurrentSelection = useCallback((selection: DashboardSelection) => {
    const current = selectionRef.current;
    return current?.key === selection.key && current.generation === selection.generation;
  }, []);
  const invalidateDetails = useCallback(() => {
    detailGeneration.current += 1;
  }, []);
  const isCurrentDetailRequest = useCallback(
    (selection: DashboardSelection, generation: number) =>
      isCurrentSelection(selection) && generation === detailGeneration.current,
    [isCurrentSelection],
  );

  const run = useCallback(async (label: string, work: () => Promise<void>) => {
    if (working.current) return;
    working.current = true;
    setBusy(label);
    setError("");
    setMessage("");
    try {
      await work();
    } catch (e) {
      setError(String(e));
    } finally {
      working.current = false;
      setBusy(null);
      if (pendingLocalRefresh.current) {
        pendingLocalRefresh.current = false;
        queueMicrotask(() => refreshLocalRef.current());
      }
    }
  }, []);
  const clearPublisher = useCallback(() => {
    inventoryGeneration.current += 1;
    const current = inventoryRef.current;
    updateInventory(
      current
        ? {
            ...current,
            source: "local",
            error: null,
            rows: current.rows.filter((row) => !!row.folder),
          }
        : null,
    );
    updateSelection(null);
  }, [updateInventory, updateSelection]);
  const loadDetail = useCallback(
    async (slug: string, selection: DashboardSelection) => {
      const generation = ++detailGeneration.current;
      try {
        const next = await api.dashboardDetail(slug);
        if (!isCurrentDetailRequest(selection, generation)) return;
        setDetail(next);
        setDetailError("");
        return next;
      } catch (e) {
        if (!isCurrentDetailRequest(selection, generation)) return;
        setDetailError(String(e));
      }
    },
    [isCurrentDetailRequest],
  );
  const loadSelectedDetail = useCallback(
    (row: InventoryRow) => {
      const selection = selectionRef.current;
      if (!selection || selection.key !== dashboardRowKey(row)) return Promise.resolve(undefined);
      return loadDetail(row.slug, selection);
    },
    [loadDetail],
  );
  const loadInventory = useCallback(
    async (folder?: string, reset = false, localOnly = false) => {
      const generation = localOnly
        ? ++localInventoryGeneration.current
        : ++inventoryGeneration.current;
      const isCurrent = () =>
        localOnly
          ? generation === localInventoryGeneration.current
          : generation === inventoryGeneration.current;
      let next: DashboardInventory;
      try {
        next = await api.dashboardInventory(folder, localOnly);
      } catch (e) {
        if (!isCurrent()) return;
        throw e;
      }
      if (!isCurrent()) return;
      const previous = inventoryRef.current;
      let merged = next;
      if (localOnly && previous) {
        const seen = new Set<string>();
        const localRows = next.rows.map((row) => {
          const old = previous.rows.find(
            (candidate) =>
              dashboardRowKey(candidate) === dashboardRowKey(row) ||
              (candidate.hosted &&
                ((row.folder && candidate.folder === row.folder) ||
                  (!row.folder && row.localState && candidate.slug === row.slug))),
          );
          if (old) seen.add(dashboardRowKey(old));
          return old
            ? {
                ...row,
                hosted: old.hosted,
                location: old.hosted && row.folder ? ("both" as const) : row.location,
                differentKey: old.differentKey,
              }
            : row;
        });
        const retainedHosted = previous.rows.filter(
          (row) =>
            row.hosted &&
            !seen.has(dashboardRowKey(row)) &&
            !next.rows.some((local) => local.localState && local.slug === row.slug),
        );
        merged = {
          ...previous,
          checkedAt:
            previous.source === "hosted" || previous.source === "stale"
              ? previous.checkedAt
              : next.checkedAt,
          teams: next.teams,
          rows: [...localRows, ...retainedHosted],
        };
      } else if (
        !reset &&
        next.source === "unavailable" &&
        previous &&
        (previous.source === "hosted" || previous.source === "stale")
      ) {
        const locals = new Map(next.rows.map((row) => [dashboardRowKey(row), row]));
        const rows = previous.rows.map((row) => {
          const key = dashboardRowKey(row);
          const local = locals.get(key);
          locals.delete(key);
          return local
            ? { ...row, folder: local.folder, localState: local.localState, error: local.error }
            : row;
        });
        merged = {
          ...previous,
          source: "stale",
          error: next.error,
          rows: [...rows, ...locals.values()],
        };
      }
      updateInventory(merged);
      const selection = selectionRef.current;
      const selectedRow = selection
        ? merged.rows.find((row) => dashboardRowKey(row) === selection.key)
        : null;
      if (
        !localOnly &&
        selection &&
        selectedRow &&
        !selectedRow.differentKey &&
        (selectedRow.hosted || selectedRow.localState === "published")
      )
        return loadDetail(selectedRow.slug, selection);
    },
    [loadDetail, updateInventory],
  );
  const refreshHosted = useCallback(async () => {
    const generation = ++keyStatusGeneration.current;
    const status = await api.dashboardPublisherKeyStatus();
    if (generation !== keyStatusGeneration.current) return;
    const changed = status.hint !== keyHint.current;
    if (changed) clearPublisher();
    keyHint.current = status.hint;
    setKey(status);
    await loadInventory(undefined, changed);
  }, [clearPublisher, loadInventory]);
  const refresh = useCallback(async () => {
    const wasFirstLoad = firstLoad.current;
    if (wasFirstLoad) {
      firstLoad.current = false;
      const generation = ++keyStatusGeneration.current;
      setBusy("Refreshing dashboards…");
      const localInventory = loadInventory(undefined, true, true).catch((e) => {
        if (generation === keyStatusGeneration.current) setError(String(e));
      });
      try {
        const status = await api.dashboardPublisherKeyStatus();
        if (generation !== keyStatusGeneration.current) return;
        const changed = keyHint.current !== null && status.hint !== keyHint.current;
        if (changed) clearPublisher();
        keyHint.current = status.hint;
        setKey(status);
        await localInventory;
        if (generation !== keyStatusGeneration.current) return;
        const inventoryGenerationAtStart = inventoryGeneration.current + 1;
        void loadInventory(undefined, changed).catch((e) => {
          if (inventoryGenerationAtStart === inventoryGeneration.current) setError(String(e));
        });
      } catch (e) {
        if (generation === keyStatusGeneration.current) setError(String(e));
      } finally {
        if (!working.current) setBusy(null);
      }
      return;
    }
    await run("Refreshing…", refreshHosted);
  }, [run, clearPublisher, loadInventory, refreshHosted]);
  const refreshFromFocus = useCallback(async () => {
    if (firstLoad.current) {
      await refresh();
      return;
    }
    if (working.current) return;
    const generation = keyStatusGeneration.current + 1;
    try {
      await refreshHosted();
    } catch (e) {
      if (generation === keyStatusGeneration.current) setError(String(e));
    }
  }, [refresh, refreshHosted]);
  useEffect(() => {
    void refresh();
  }, [refresh]);

  const refreshLocal = useCallback(() => {
    if (working.current) {
      pendingLocalRefresh.current = true;
      return Promise.resolve();
    }
    return run("Refreshing dashboards…", async () => {
      await loadInventory(undefined, false, true);
    });
  }, [run, loadInventory]);
  useEffect(() => {
    refreshLocalRef.current = () => void refreshLocal();
    return () => {
      pendingLocalRefresh.current = false;
    };
  }, [refreshLocal]);
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    void listen<ContextIndexChangedPayload>(CONTEXT_INDEX_CHANGED_EVENT, (event) => {
      const change = event.payload;
      if (
        !change ||
        (!change.truncated && !change.paths.some((path) => dashboardManifestPath.test(path)))
      )
        return;
      clearTimeout(timer);
      timer = setTimeout(() => void refreshLocal(), 180);
    }).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });
    const onFocus = () => void refreshFromFocus();
    const onVisibility = () => {
      if (document.visibilityState === "visible") onFocus();
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      disposed = true;
      clearTimeout(timer);
      unlisten?.();
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [refreshFromFocus]);

  const select = (row: InventoryRow, key = dashboardRowKey(row)) =>
    run("Loading dashboard…", async () => {
      const selection = updateSelection(key);
      const currentRow =
        inventoryRef.current?.rows.find((candidate) => dashboardRowKey(candidate) === key) ?? row;
      const canLoadDetail =
        !currentRow.differentKey && (!!currentRow.hosted || currentRow.localState === "published");
      if (canLoadDetail && selection) await loadDetail(currentRow.slug, selection);
    });
  const close = () => updateSelection(null);
  const saveKey = (value: string) =>
    run("Saving…", async () => {
      if (!value.trim().startsWith("csp_"))
        throw new Error("This doesn't look like a publishing key. They start with csp_.");
      const generation = ++keyStatusGeneration.current;
      const status = await api.dashboardPublisherKeySave(value);
      if (generation !== keyStatusGeneration.current) return;
      clearPublisher();
      setKey(status);
      keyHint.current = status.hint;
      await loadInventory(undefined, true);
    });
  const removeKey = () =>
    run("Removing key…", async () => {
      const generation = ++keyStatusGeneration.current;
      const status = await api.dashboardPublisherKeyRemove();
      if (generation !== keyStatusGeneration.current) return;
      clearPublisher();
      setKey(status);
      keyHint.current = status.hint;
      await loadInventory(undefined, true);
    });
  const openFolder = (folder: string) =>
    run("Opening folder…", async () => {
      await loadInventory(folder);
    });
  const publish = (row: InventoryRow) =>
    run("Publishing…", async () => {
      if (!row.folder) throw new Error("Open the source folder to update this dashboard.");
      await api.dashboardInventory(row.folder);
      const result = await api.dashboardPublish(row.folder);
      invalidateDetails();
      await loadInventory();
      if (result.hostedCopy !== "live")
        throw new Error(
          `Didn't publish. ${result.hostedCopyError ?? "The server did not confirm publication. Try again."}`,
        );
      setMessage("Published. Viewers see it on their next load.");
    });
  const unpublish = (row: InventoryRow) =>
    run("Unpublishing…", async () => {
      const result = await api.dashboardUnpublish(row.slug, row.folder);
      invalidateDetails();
      await loadInventory();
      if (result.hostedCopy !== "removed")
        throw new Error(
          `Couldn't confirm unpublish. ${result.hostedCopyError ?? "The link may still work. Try again."}`,
        );
      setMessage("Unpublished. The link no longer works.");
    });
  const share = (row: InventoryRow, add: string[], remove: string[]) =>
    run(remove.length ? "Removing…" : "Adding…", async () => {
      let result;
      try {
        result = await api.dashboardShare(row.slug, add, remove);
        invalidateDetails();
      } catch (error) {
        if (!remove.length) throw new Error(`Couldn't add viewer: ${String(error)}`);
        let refreshed: DashboardDetail | undefined;
        try {
          refreshed = await loadInventory();
          if (!refreshed) refreshed = await loadSelectedDetail(row);
        } catch {
          refreshed = await loadSelectedDetail(row);
        }
        const stillListed = refreshed?.dashboard.shares.some((viewer) =>
          remove.some((email) => email.trim().toLowerCase() === viewer.email.toLowerCase()),
        );
        if (refreshed && !stillListed) {
          setMessage("Viewer access removed (confirmed by refreshed list).");
          return;
        }
        throw new Error(
          `Couldn't confirm viewer removal: ${String(error)}. ${
            refreshed
              ? "The refreshed viewer list still includes that person. Try again."
              : "Refresh to check their current access before trying again."
          }`,
        );
      }
      setDetail((current) =>
        current
          ? { ...current, dashboard: { ...current.dashboard, shares: result.shares } }
          : current,
      );
      setMessage(
        remove.length
          ? "Viewer access removed."
          : "Viewer access updated. Check the invitation status below.",
      );
      await loadInventory();
    });
  const signOutViewer = (row: InventoryRow, shareId: string) =>
    run("Signing out…", async () => {
      await api.dashboardViewerSignOut(row.slug, shareId);
      invalidateDetails();
      await loadSelectedDetail(row);
    });
  return {
    key,
    inventory,
    selected,
    detail,
    detailError,
    busy,
    error,
    message,
    refresh,
    refreshLocal,
    select,
    close,
    saveKey,
    removeKey,
    openFolder,
    publish,
    unpublish,
    share,
    signOutViewer,
  };
}
