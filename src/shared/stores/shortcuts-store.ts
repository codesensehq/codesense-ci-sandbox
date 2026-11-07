// SPDX-License-Identifier: MIT
import { create } from "zustand";

import { logger } from "@/shared/lib/logger";
import { createDebouncedAction } from "@/shared/lib/debounced-action";
import {
  formatKeystroke,
  formatKeystrokeDisplay,
  isReservedForDashboardCommand,
  isReservedKeystroke,
  parseKeystroke,
} from "@/shared/lib/shortcuts/keystroke";
import {
  findConflict,
  isDashboardCommandActionId,
  resolveBindings,
  shortcutLabelById,
  SHORTCUT_ACTIONS,
  useDynamicShortcutActions,
} from "@/shared/lib/shortcuts/registry";
import { saveAppState } from "@/tauri/commands";
import { serializeAppStateWrite } from "@/shared/lib/app-state-write";
import { fieldPatch } from "@/shared/lib/field-patch";
import { readPrefsState } from "@/features/dashboard/lib/prefs-client";
// NOTE: panel-menu imports useShortcutsStore back — a deliberate import cycle
// that is safe because BOTH sides use the other only at call time (never at
// module-eval). Keep it that way: do not reference rebuildNativeMenu at module
// top-level here, nor useShortcutsStore at module top-level in panel-menu.
import { rebuildNativeMenu } from "@/features/shell/right-pane/panel-menu";

/**
 * Shortcut-override store. Holds only user-overridden keystrokes —
 * defaults live on the registry. `getEffectiveShortcut(id)` returns the
 * override when present, the registry default otherwise. Persisted to
 * the same app-state file as `useViewerStore` / `useOrgStore` under the
 * `shortcuts` key.
 *
 * An override of `""` means "explicitly no chord", for built-ins and dashboard
 * commands alike. Overrides of dashboard commands are kept while the dashboard is
 * uninstalled, so reinstalling it restores them.
 *
 * `setShortcut` validates against reserved keys + conflicts and returns
 * a `SetResult` discriminated union; `takeShortcut` is the explicit take-over
 * that gives a chord to one action and unbinds its previous owner. The Settings UI displays the
 * rejection inline; programmatic callers assert on the result shape.
 * The store never throws on invalid input.
 */

export type SetResult =
  | { ok: true }
  | { ok: false; reason: "reserved"; detail: string }
  | { ok: false; reason: "conflict"; detail: string; conflictsWith: string }
  | { ok: false; reason: "invalid"; detail: string };

interface PersistedShortcutsState {
  version: number;
  shortcuts: Record<string, string>;
}

/**
 * Current persisted-shortcuts schema version. Bumped whenever an action
 * ID is renamed so `migratePersistedShortcuts` can carry old overrides
 * forward. v1 covers the T.6 rename chain; v2 adds the
 * scratch-note → right-pane toggle rename (see RENAMES below).
 */
const CURRENT_SHORTCUTS_VERSION = 2;

/**
 * Action-ID renames to replay against pre-version blobs, oldest first.
 * T.6 (commit 5b4a8f4) renamed two IDs through the shared
 * `nav.switch-workspace` key:
 *   - old `nav.switch-workspace` (org switcher)  → `nav.switch-org`
 *   - old `nav.switch-repo`      (repo switcher) → `nav.switch-workspace`
 * The second entry's target collides with the first entry's source, so
 * this is a swap — `migratePersistedShortcuts` reads every `from` off the
 * immutable input map before writing any `to`, so order can't clobber.
 * The next rename is one more row here + a version bump.
 */
const RENAMES: readonly { from: string; to: string }[] = [
  { from: "nav.switch-workspace", to: "nav.switch-org" },
  { from: "nav.switch-repo", to: "nav.switch-workspace" },
  // v2: the right pane hosts more than scratch notes, so the toggle id was
  // generalized off "scratch-note".
  { from: "viewer.toggle-scratch-note", to: "viewer.toggle-right-pane" },
];

/** Source IDs of every rename — a target that is also a source is being
 *  vacated, so it never counts as a surviving override to preserve. */
const RENAME_SOURCE_IDS = new Set(RENAMES.map((r) => r.from));

/**
 * Carry persisted overrides across action-ID renames. Pure: no store or
 * IO access, so it unit-tests in isolation. Operates on the RAW stored map
 * (before unknown-ID filtering) because the renamed `from` IDs are no
 * longer in the registry and would otherwise be dropped.
 *
 * Reads all sources off the original `shortcuts` before writing any
 * target, so a key that is both a rename source and a rename target (the
 * `nav.switch-workspace` swap) resolves correctly regardless of row order.
 * An explicit existing override on a target wins over a migrated value.
 */
export function migratePersistedShortcuts(persisted: {
  version?: number;
  shortcuts: Record<string, string>;
}): { shortcuts: Record<string, string>; didMigrate: boolean } {
  const source = persisted.shortcuts;
  // Coerce a non-numeric version (hand-edited / corrupted blob) to 0 so the
  // gate can't be tricked by a string/bool into skipping a needed migration.
  const version = typeof persisted.version === "number" ? persisted.version : 0;
  // Bail before the `in`-operator loop on a malformed (non-plain-object)
  // shortcuts value — `"x" in "string"` throws. The caller's normalize loop
  // discards whatever we return here.
  if (version >= CURRENT_SHORTCUTS_VERSION || typeof source !== "object" || source === null) {
    return { shortcuts: source, didMigrate: false };
  }
  const next = { ...source };
  let didMigrate = false;
  for (const { from, to } of RENAMES) {
    if (!(from in source)) continue;
    delete next[from];
    // Carry the value forward unless the target already holds an explicit
    // override that is itself NOT being vacated by another rename. A target
    // that is also a rename source (the nav.switch-workspace swap) is being
    // moved away, so it's fair game to overwrite.
    const targetHasSurvivingOverride = to in source && !RENAME_SOURCE_IDS.has(to);
    if (!targetHasSurvivingOverride) next[to] = source[from];
    didMigrate = true;
  }
  return { shortcuts: next, didMigrate };
}

/** Two actions bound to the same keystroke. */
export interface BindingCollision {
  keystroke: string;
  actionIds: [string, string];
}

/**
 * Keystrokes that more than one action resolves to, given the user's overrides.
 * `setShortcut` refuses a conflicting override, so a collision can only come from
 * an override saved before a later release gave another action that key as its
 * default (⌘⇧T moving to Reopen Closed Tab, say). The first action in registry
 * order wins the key; the other is unreachable until rebound. A dashboard
 * command's default never collides: it registers unbound instead.
 */
export function findBindingCollisions(shortcuts: Record<string, string>): BindingCollision[] {
  const byKey = new Map<string, string>();
  const collisions: BindingCollision[] = [];
  for (const [id, { keystroke }] of resolveBindings(shortcuts)) {
    if (!keystroke) continue;
    const first = byKey.get(keystroke);
    if (first) collisions.push({ keystroke, actionIds: [first, id] });
    else byKey.set(keystroke, id);
  }
  return collisions;
}

/** A take-over: the chord moved to `actionId`; `displaced` lost it. */
export type TakeResult =
  | { ok: true; keystroke: string; displaced: string | null; previous: Record<string, string> }
  | Exclude<SetResult, { ok: true } | { reason: "conflict" }>;

interface ShortcutsState {
  shortcuts: Record<string, string>;
  stateLoaded: boolean;
  setShortcut: (actionId: string, keystroke: string) => SetResult;
  /** Give `keystroke` to `actionId` even when another action holds it; that action
   *  is explicitly unbound (`""`). `previous` restores both on Undo. */
  takeShortcut: (actionId: string, keystroke: string) => TakeResult;
  /** Store "explicitly no chord" for an action. */
  clearShortcut: (actionId: string) => void;
  /** Put back a whole override map (Undo of a take-over). */
  restoreShortcuts: (shortcuts: Record<string, string>) => void;
  /** Back to the default. True when a dashboard command gave that default chord back. */
  resetShortcut: (actionId: string) => boolean;
  resetAll: () => void;
  persistState: () => void;
  loadPersistedState: (data?: { shortcuts?: unknown }) => Promise<void>;
}

let pendingShortcutPatch: Record<string, string | null> = {};
const debouncedPersist = createDebouncedAction(
  () =>
    serializeAppStateWrite(async () => {
      try {
        const patch = { ...pendingShortcutPatch };
        const persisted: PersistedShortcutsState = {
          version: CURRENT_SHORTCUTS_VERSION,
          shortcuts: patch as Record<string, string>,
        };
        await saveAppState(JSON.stringify({ shortcuts: persisted }), true);
        for (const key of Object.keys(patch)) {
          if (pendingShortcutPatch[key] === patch[key]) delete pendingShortcutPatch[key];
        }
        logger.info("shortcuts-store", "persistState: success");
      } catch (e) {
        logger.error("shortcuts-store", `persistState: failed — ${e}`);
        throw e;
      }
    }),
  300,
  "persistState",
);

/**
 * Push the current effective override map to Rust so the native macOS
 * menu renders accelerators that reflect live rebinds. No-op on non-
 * macOS (the Rust command short-circuits). Fire-and-forget; failures
 * are logged here — the frontend keyboard handler is authoritative
 * regardless of whether the menu display succeeds.
 */
function triggerMenuRebuild(): void {
  // SW-RP-1: route through rebuildNativeMenu so a shortcut rebind keeps the
  // registry-driven View → Panels submenu intact (and vice-versa).
  rebuildNativeMenu();
}

/** Validate and canonicalize a candidate chord for `actionId` the way every write
 *  does: a dashboard command checks the active host's reserved chords. */
function canonicalChord(
  actionId: string,
  keystroke: string,
): { ok: true; canonical: string } | Exclude<SetResult, { ok: true } | { reason: "conflict" }> {
  let canonical: string;
  try {
    canonical = formatKeystroke(parseKeystroke(keystroke));
  } catch (e) {
    return { ok: false, reason: "invalid", detail: e instanceof Error ? e.message : String(e) };
  }
  const reserved = isDashboardCommandActionId(actionId)
    ? isReservedForDashboardCommand(canonical)
    : isReservedKeystroke(canonical);
  if (reserved) return { ok: false, reason: "reserved", detail: reserved.message };
  return { ok: true, canonical };
}

const NOT_LOADED = { ok: false, reason: "invalid", detail: "Shortcuts not loaded yet" } as const;

export const useShortcutsStore = create<ShortcutsState>((set, get) => ({
  shortcuts: {},
  stateLoaded: false,

  setShortcut: (actionId, keystroke) => {
    // Reject writes before rehydrate lands. The alternative (accept
    // and hope persistState-skip avoids clobber) is brittle: a late
    // loadPersistedState will overwrite the in-memory value. Callers
    // only hit this path during a narrow startup window; the Settings
    // UI only renders after the app is initialized, so real users
    // never see it.
    if (!get().stateLoaded) return NOT_LOADED;

    // Normalize the candidate through parse + format so storage is
    // always canonical regardless of how the caller built the string.
    const chord = canonicalChord(actionId, keystroke);
    if (!chord.ok) return chord;
    const { canonical } = chord;

    const current = get().shortcuts;
    const conflict = findConflict(canonical, current, actionId);
    if (conflict) {
      return {
        ok: false,
        reason: "conflict",
        detail: `${formatKeystrokeDisplay(canonical)} is used by "${shortcutLabelById(conflict)}"`,
        conflictsWith: conflict,
      };
    }

    get().restoreShortcuts({ ...current, [actionId]: canonical });
    return { ok: true };
  },

  takeShortcut: (actionId, keystroke) => {
    if (!get().stateLoaded) return NOT_LOADED;
    const chord = canonicalChord(actionId, keystroke);
    if (!chord.ok) return chord;
    const previous = get().shortcuts;
    const displaced = findConflict(chord.canonical, previous, actionId);
    get().restoreShortcuts({
      ...previous,
      [actionId]: chord.canonical,
      ...(displaced ? { [displaced]: "" } : {}),
    });
    return { ok: true, keystroke: chord.canonical, displaced, previous };
  },

  clearShortcut: (actionId) => {
    if (!get().stateLoaded || get().shortcuts[actionId] === "") return;
    get().restoreShortcuts({ ...get().shortcuts, [actionId]: "" });
  },

  restoreShortcuts: (shortcuts) => {
    Object.assign(pendingShortcutPatch, fieldPatch(get().shortcuts, shortcuts) ?? {});
    set({ shortcuts });
    get().persistState();
    triggerMenuRebuild();
  },

  resetShortcut: (actionId) => {
    const current = get().shortcuts;
    if (!(actionId in current)) return false;
    const next = { ...current };
    delete next[actionId];
    // A built-in's default wins over dashboard commands, so one that took the chord
    // gives it back and returns to its own default (which then conflicts, unbound).
    const action = SHORTCUT_ACTIONS.find((a) => a.id === actionId);
    let reclaimed = false;
    for (const [id, keystroke] of Object.entries(current)) {
      if (action && isDashboardCommandActionId(id) && keystroke === action.defaultKeystroke) {
        delete next[id];
        reclaimed = true;
      }
    }
    get().restoreShortcuts(next);
    return reclaimed;
  },

  resetAll: () => {
    if (Object.keys(get().shortcuts).length === 0) return;
    get().restoreShortcuts({});
  },

  persistState: () => {
    // Suppress writes until rehydrate completes, matching the org-store
    // convention: pre-rehydrate setters must not schedule a debounced
    // save that could race the async load and overwrite disk state.
    if (!get().stateLoaded) return;
    debouncedPersist.trigger();
  },

  loadPersistedState: async (snapshot) => {
    logger.info("shortcuts-store", "loadPersistedState: loading");
    if (!get().stateLoaded) pendingShortcutPatch = {};
    try {
      const data = snapshot ?? (await readPrefsState());
      const persisted = data.shortcuts as Partial<PersistedShortcutsState> | undefined;
      if (persisted && persisted.shortcuts) {
        // Carry overrides across action-ID renames BEFORE the unknown-ID
        // filter below — the renamed source IDs are gone from the registry
        // and would otherwise be dropped as unknown.
        const { shortcuts: migrated, didMigrate } = migratePersistedShortcuts({
          version: persisted.version,
          shortcuts: persisted.shortcuts,
        });
        // Normalize every loaded entry through parse + format so stale
        // formats in the stored file are brought up to canonical form.
        const validIds = new Set(SHORTCUT_ACTIONS.map((a) => a.id));
        const clean: Record<string, string> = {};
        for (const [id, keystroke] of Object.entries(migrated)) {
          if (!validIds.has(id) && !isDashboardCommandActionId(id)) continue;
          if (typeof keystroke !== "string") continue;
          if (keystroke === "") {
            clean[id] = "";
            continue;
          }
          try {
            clean[id] = formatKeystroke(parseKeystroke(keystroke));
          } catch {
            // Drop unparseable entries silently — they'll fall back to
            // the registry default.
          }
        }
        if (didMigrate)
          Object.assign(pendingShortcutPatch, fieldPatch(persisted.shortcuts, clean) ?? {});
        for (const [key, value] of Object.entries(pendingShortcutPatch)) {
          if (value === null) delete clean[key];
          else clean[key] = value;
        }
        set({ shortcuts: clean, stateLoaded: true });
        for (const { keystroke, actionIds } of findBindingCollisions(clean)) {
          logger.warn(
            "shortcuts-store",
            `${keystroke} is bound to both ${actionIds[0]} and ${actionIds[1]}; ${actionIds[0]} gets it — rebind one in Settings → Shortcuts`,
          );
        }
        // Persist the upgraded blob so the version stamp lands and the
        // migration never re-runs. Reuses the existing debounced path —
        // no extra synchronous write, no added startup blocking. Ordered
        // after the stateLoaded flip so the persist guard doesn't drop it.
        if (didMigrate) get().persistState();
      } else {
        set({
          shortcuts: Object.fromEntries(
            Object.entries(pendingShortcutPatch).filter(
              (entry): entry is [string, string] => entry[1] !== null,
            ),
          ),
          stateLoaded: true,
        });
      }
    } catch (e) {
      logger.error("shortcuts-store", `loadPersistedState: failed — ${e}`);
      set({ stateLoaded: true });
    }
    // Push the effective overrides to the Rust menu so accelerator
    // display reflects the user's rebinds on first paint. Fire-and-
    // forget — macOS-only, and the keyboard handler is already live
    // regardless of whether the menu renders the hint.
    triggerMenuRebuild();
  },
}));

/**
 * Resolve the keystroke for an action id: user override if present,
 * registry default otherwise. Reads the store directly so callers can
 * use it in non-React contexts (the keyboard handler, the event bridge,
 * menu click dispatcher). React components should prefer
 * `useEffectiveShortcut` for reactive re-render on change.
 *
 * Unknown ids return an empty string + warn rather than throwing —
 * callers that render the result into a tooltip shouldn't crash the
 * UI over a stale persisted id after a version downgrade.
 */
export function getEffectiveShortcut(actionId: string): string {
  const binding = resolveBindings(useShortcutsStore.getState().shortcuts).get(actionId);
  if (!binding) {
    logger.warn("shortcuts-store", `getEffectiveShortcut: unknown action id "${actionId}"`);
    return "";
  }
  return binding.keystroke;
}

/**
 * React hook form of `getEffectiveShortcut`. Subscribes to the store so
 * the component re-renders when the user rebinds the action. Used by
 * tooltip labels that must track overrides live.
 */
export function useEffectiveShortcut(actionId: string): string {
  // Primitive selectors, so a rebind elsewhere or a dashboard's commands changing
  // re-renders only the components whose chord actually changed.
  const keystroke = (shortcuts: Record<string, string>) =>
    resolveBindings(shortcuts).get(actionId)?.keystroke ?? "";
  useDynamicShortcutActions(() => keystroke(useShortcutsStore.getState().shortcuts));
  return useShortcutsStore((s) => keystroke(s.shortcuts));
}

/** The action's resolved binding, with why it is unbound; re-renders on any
 *  rebind and whenever dashboard commands come or go. */
export function useResolvedBinding(actionId: string) {
  const shortcuts = useShortcutsStore((s) => s.shortcuts);
  useDynamicShortcutActions((s) => s.all);
  const binding = resolveBindings(shortcuts).get(actionId);
  if (!binding)
    logger.warn("shortcuts-store", `useResolvedBinding: unknown action id "${actionId}"`);
  return binding;
}

export function applyShortcutPrefs(value: unknown) {
  void useShortcutsStore.getState().loadPersistedState({ shortcuts: value });
}

/** Finish accepted global edits before changing the writer principal. */
export async function flushShortcutsForProfileSwitch(): Promise<void> {
  if (!useShortcutsStore.getState().stateLoaded) return;
  debouncedPersist.trigger();
  await debouncedPersist.flushStrict();
}
