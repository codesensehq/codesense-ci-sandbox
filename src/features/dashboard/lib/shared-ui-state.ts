// SPDX-License-Identifier: MIT
import { readDashboardPrefs, writeDashboardPrefs } from "./dashboard-prefs";
import { normalizeNavLens, type NavLensState } from "@/shared/lib/nav-lens";

const UI_STATE_KEY = "liveHubUi";
const LENS_FIELDS = [
  "source",
  "view",
  "lens",
  "arrange",
  "storiesOnly",
  "doneOpen",
  "historyBranches",
] as const;

/** Slot-only fields live in prefs; native lens and expansion state have one owner. */
export async function readSharedUiState(key: string): Promise<Record<string, unknown>> {
  const state = await readDashboardPrefs(key, UI_STATE_KEY);
  const teamKey = fileNavTeamKey(key);
  if (!teamKey) return state;
  const { useViewerStore } = await import("@/shared/stores/viewer-store");
  const viewer = useViewerStore.getState();
  const lens = normalizeNavLens(viewer.navLens[teamKey]);
  const scope =
    lens.source.kind === "private"
      ? "private"
      : lens.source.kind === "clone" &&
          typeof state.scope === "string" &&
          /^(story|agent):/.test(state.scope)
        ? state.scope
        : "team";
  return { ...state, ...lens, scope, expanded: viewer.statusNavExpandedPaths[teamKey] ?? [] };
}
export async function writeSharedUiState(
  key: string,
  patch: Record<string, unknown>,
): Promise<Record<string, unknown>> {
  const teamKey = fileNavTeamKey(key);
  if (!teamKey) return writeDashboardPrefs(key, patch, UI_STATE_KEY);
  const { useViewerStore } = await import("@/shared/stores/viewer-store");
  const { expanded, ...rest } = patch;
  const lens: Partial<NavLensState> = {};
  for (const field of LENS_FIELDS) {
    if (field in rest) Object.assign(lens, { [field]: rest[field] });
    delete rest[field];
  }
  // A scope-only write still supports the existing files pane's source picker.
  if (!lens.source && typeof rest.scope === "string")
    lens.source = { kind: rest.scope === "private" ? "private" : "clone" };
  if (Object.keys(lens).length) useViewerStore.getState().setNavLens(teamKey, lens);
  if (lens.source && !("scope" in rest))
    rest.scope = lens.source.kind === "private" ? "private" : "team";
  await writeDashboardPrefs(key, rest, UI_STATE_KEY);
  if (Array.isArray(expanded)) {
    useViewerStore.setState((state) => ({
      statusNavExpandedPaths: { ...state.statusNavExpandedPaths, [teamKey]: expanded as string[] },
    }));
    useViewerStore.getState().persistState();
  }
  return readSharedUiState(key);
}

/** Every native lens setter, including MCP and menus, invalidates the shared UI view. */
export async function watchSharedUiState(changed: (topic: string, viewKey: string) => void) {
  const { useViewerStore } = await import("@/shared/stores/viewer-store");
  return useViewerStore.subscribe((next, previous) => {
    const keys = new Set([
      ...Object.keys(next.navLens),
      ...Object.keys(previous.navLens),
      ...Object.keys(next.statusNavExpandedPaths),
      ...Object.keys(previous.statusNavExpandedPaths),
    ]);
    for (const teamKey of keys) {
      if (
        next.navLens[teamKey] === previous.navLens[teamKey] &&
        next.statusNavExpandedPaths[teamKey] === previous.statusNavExpandedPaths[teamKey]
      )
        continue;
      const at = teamKey.lastIndexOf(":");
      const viewKey = `team:${teamKey.slice(0, at)}/${teamKey.slice(at + 1)}`;
      if (next.navLens[teamKey]?.source !== previous.navLens[teamKey]?.source) {
        void writeDashboardPrefs(
          JSON.stringify(["ui:file-nav", viewKey]),
          {
            scope: next.navLens[teamKey]?.source.kind === "private" ? "private" : "team",
          },
          UI_STATE_KEY,
        )
          .then(() => changed("ui:file-nav", viewKey))
          .catch(console.error);
      } else changed("ui:file-nav", viewKey);
    }
  });
}
export function fileNavTeamKey(key: string): string | null {
  if (!key.startsWith("[")) return null;
  const [topic, view] = JSON.parse(key) as [string, string];
  if (topic !== "ui:file-nav" || !view.startsWith("team:")) return null;
  const at = view.lastIndexOf("/");
  return `${view.slice(5, at)}:${view.slice(at + 1)}`;
}
