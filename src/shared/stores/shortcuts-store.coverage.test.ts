// SPDX-License-Identifier: MIT
vi.mock("@/features/dashboard/lib/prefs-client", () => ({
  readPrefsState: async () => JSON.parse((await mockLoadAppState()) || "{}"),
}));
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { serializeAppStateWrite } from "@/shared/lib/app-state-write";

// ---------- Tauri command mocks ----------

const mockLoadAppState = vi.fn<() => Promise<string>>();
const mockSaveAppState = vi.fn<(state: string, patch?: boolean) => Promise<void>>();
const mockRebuildMenu = vi.fn<(overrides: Record<string, string>) => Promise<void>>();

vi.mock("@/tauri/commands", () => ({
  loadAppState: () => mockLoadAppState(),
  saveAppState: (state: string, patch?: boolean) => mockSaveAppState(state, patch),
  rebuildMenu: (overrides: Record<string, string>) => mockRebuildMenu(overrides),
}));

const { mockWarn } = vi.hoisted(() => ({ mockWarn: vi.fn() }));
vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: mockWarn },
}));

// ---------- Imports (after mocks) ----------

import {
  findBindingCollisions,
  getEffectiveShortcut,
  migratePersistedShortcuts,
  useShortcutsStore,
} from "./shortcuts-store";

// ---------- Setup ----------

beforeEach(async () => {
  mockLoadAppState.mockReset();
  mockSaveAppState.mockReset();
  mockRebuildMenu.mockReset();
  mockLoadAppState.mockResolvedValue("{}");
  mockSaveAppState.mockResolvedValue(undefined);
  mockRebuildMenu.mockResolvedValue(undefined);
  // Reset store state between tests
  useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
  await useShortcutsStore
    .getState()
    .loadPersistedState({ shortcuts: { version: 2, shortcuts: {} } });
  mockRebuildMenu.mockClear();
});

afterEach(() => {
  vi.useRealTimers();
});

// ---------- Tests ----------

it("queues shortcut persistence behind other app-state writers", async () => {
  vi.useFakeTimers();
  let release!: () => void;
  const barrier = serializeAppStateWrite(
    () =>
      new Promise<void>((resolve) => {
        release = resolve;
      }),
  );
  await Promise.resolve();
  try {
    useShortcutsStore.getState().setShortcut("settings.open", "CmdOrCtrl+Shift+Y");
    await vi.advanceTimersByTimeAsync(350);
    expect(mockSaveAppState).not.toHaveBeenCalled();
  } finally {
    release();
    await barrier;
    await vi.advanceTimersByTimeAsync(1);
  }
  expect(mockSaveAppState).toHaveBeenCalledTimes(1);
  expect(mockSaveAppState.mock.calls[0][1]).toBe(true);
});

describe("useShortcutsStore — setShortcut", () => {
  it("accepts a valid keystroke and updates the map", () => {
    const result = useShortcutsStore.getState().setShortcut("settings.open", "CmdOrCtrl+Shift+.");
    expect(result).toEqual({ ok: true });
    expect(useShortcutsStore.getState().shortcuts["settings.open"]).toBe("CmdOrCtrl+Shift+.");
  });

  it("normalizes the letter key to uppercase via parse + format", () => {
    // Arbitrary key for the normalization check (nav.open-workspace is ⌘K post-SW-FX-49).
    const result = useShortcutsStore.getState().setShortcut("settings.open", "CmdOrCtrl+Shift+y");
    expect(result).toEqual({ ok: true });
    expect(useShortcutsStore.getState().shortcuts["settings.open"]).toBe("CmdOrCtrl+Shift+Y");
  });

  it("rejects a reserved keystroke (CmdOrCtrl+Q)", () => {
    const result = useShortcutsStore.getState().setShortcut("settings.open", "CmdOrCtrl+Q");
    expect(result).toMatchObject({ ok: false, reason: "reserved" });
    expect(result).not.toHaveProperty("ok", true);
    expect(useShortcutsStore.getState().shortcuts["settings.open"]).toBeUndefined();
  });

  it("rejects a conflicting keystroke and names the conflicting action", () => {
    // SW-SN-6: CmdOrCtrl+Shift+O is now nav.goto-heading (nav.switch-org moved to
    // CmdOrCtrl+Shift+G). Setting another action to Shift+O conflicts with it.
    const result = useShortcutsStore.getState().setShortcut("settings.open", "CmdOrCtrl+Shift+O");
    expect(result).toMatchObject({
      ok: false,
      reason: "conflict",
      conflictsWith: "nav.goto-heading",
    });
  });

  it("allows re-saving the same action to its CURRENT keystroke (no self-conflict)", () => {
    // Settings default is CmdOrCtrl+,
    const result = useShortcutsStore.getState().setShortcut("settings.open", "CmdOrCtrl+,");
    expect(result).toEqual({ ok: true });
  });

  it("rejects invalid keystroke strings with `reason: 'invalid'`", () => {
    const result = useShortcutsStore.getState().setShortcut("settings.open", "");
    expect(result).toMatchObject({ ok: false, reason: "invalid" });
  });

  it("rejects writes before loadPersistedState completes (stateLoaded=false)", () => {
    useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
    const result = useShortcutsStore.getState().setShortcut("settings.open", "CmdOrCtrl+.");
    expect(result).toMatchObject({ ok: false, reason: "invalid" });
    expect(useShortcutsStore.getState().shortcuts["settings.open"]).toBeUndefined();
  });

  it("triggers a macOS menu rebuild with the new overrides map", () => {
    useShortcutsStore.getState().setShortcut("settings.open", "CmdOrCtrl+Shift+.");
    expect(mockRebuildMenu).toHaveBeenCalledWith({ "settings.open": "CmdOrCtrl+Shift+." });
  });
});

describe("useShortcutsStore — resetShortcut / resetAll", () => {
  it("resetShortcut removes a single override", () => {
    useShortcutsStore.setState({
      shortcuts: { "settings.open": "CmdOrCtrl+.", "nav.switch-workspace": "CmdOrCtrl+R" },
      stateLoaded: true,
    });
    useShortcutsStore.getState().resetShortcut("settings.open");
    expect(useShortcutsStore.getState().shortcuts).toEqual({
      "nav.switch-workspace": "CmdOrCtrl+R",
    });
  });

  it("resetShortcut is a no-op when the override does not exist", () => {
    useShortcutsStore.setState({
      shortcuts: { "nav.switch-workspace": "CmdOrCtrl+R" },
      stateLoaded: true,
    });
    useShortcutsStore.getState().resetShortcut("settings.open");
    expect(useShortcutsStore.getState().shortcuts).toEqual({
      "nav.switch-workspace": "CmdOrCtrl+R",
    });
  });

  it("resetAll clears every override", () => {
    useShortcutsStore.setState({
      shortcuts: { "settings.open": "X", "nav.switch-workspace": "Y" },
      stateLoaded: true,
    });
    useShortcutsStore.getState().resetAll();
    expect(useShortcutsStore.getState().shortcuts).toEqual({});
  });

  it("resetShortcut triggers a menu rebuild with the remaining overrides", () => {
    useShortcutsStore.setState({
      shortcuts: { "settings.open": "CmdOrCtrl+.", "nav.switch-workspace": "CmdOrCtrl+R" },
      stateLoaded: true,
    });
    mockRebuildMenu.mockClear();
    useShortcutsStore.getState().resetShortcut("settings.open");
    expect(mockRebuildMenu).toHaveBeenCalledWith({ "nav.switch-workspace": "CmdOrCtrl+R" });
  });

  it("resetAll triggers a menu rebuild with an empty overrides map", () => {
    useShortcutsStore.setState({
      shortcuts: { "settings.open": "X" },
      stateLoaded: true,
    });
    mockRebuildMenu.mockClear();
    useShortcutsStore.getState().resetAll();
    expect(mockRebuildMenu).toHaveBeenCalledWith({});
  });
});

describe("getEffectiveShortcut", () => {
  it("returns the override when one is set", () => {
    useShortcutsStore.getState().restoreShortcuts({ "settings.open": "CmdOrCtrl+Shift+." });
    expect(getEffectiveShortcut("settings.open")).toBe("CmdOrCtrl+Shift+.");
  });

  it("returns the registry default when no override is set", () => {
    const action = SHORTCUT_ACTIONS.find((a) => a.id === "settings.open")!;
    expect(getEffectiveShortcut("settings.open")).toBe(action.defaultKeystroke);
  });

  it("returns an empty string (not throws) for an unknown action id", () => {
    // Unknown ids can appear from stale persisted state after a
    // version downgrade — callers should get an empty string, not a
    // crash.
    expect(getEffectiveShortcut("does-not-exist")).toBe("");
  });
});

describe("useShortcutsStore — persist / load round-trip", () => {
  it("loadPersistedState reads the shortcuts block from app state", async () => {
    mockLoadAppState.mockResolvedValue(
      JSON.stringify({ shortcuts: { shortcuts: { "settings.open": "CmdOrCtrl+Shift+." } } }),
    );
    useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
    await useShortcutsStore.getState().loadPersistedState();
    expect(useShortcutsStore.getState().shortcuts).toEqual({
      "settings.open": "CmdOrCtrl+Shift+.",
    });
    expect(useShortcutsStore.getState().stateLoaded).toBe(true);
  });

  it("loadPersistedState keeps cleared chords and uninstalled dashboards' bindings", async () => {
    const dashboard = "dashboard:team:acme:dev/inbox/archive";
    mockLoadAppState.mockResolvedValue(
      JSON.stringify({
        shortcuts: {
          version: 2,
          shortcuts: { "viewer.toggle-nav": "", [dashboard]: "CmdOrCtrl+Alt+a", "gone.id": "F1" },
        },
      }),
    );
    useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
    await useShortcutsStore.getState().loadPersistedState();
    expect(useShortcutsStore.getState().shortcuts).toEqual({
      "viewer.toggle-nav": "",
      [dashboard]: "CmdOrCtrl+Alt+A",
    });
    expect(getEffectiveShortcut("viewer.toggle-nav")).toBe("");
  });

  it("loadPersistedState normalizes stored keystrokes through parse + format", async () => {
    mockLoadAppState.mockResolvedValue(
      JSON.stringify({ shortcuts: { shortcuts: { "settings.open": "CmdOrCtrl+Shift+k" } } }),
    );
    useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
    await useShortcutsStore.getState().loadPersistedState();
    expect(useShortcutsStore.getState().shortcuts["settings.open"]).toBe("CmdOrCtrl+Shift+K");
  });

  it("loadPersistedState drops unparseable stored entries", async () => {
    mockLoadAppState.mockResolvedValue(
      JSON.stringify({
        shortcuts: {
          shortcuts: {
            "settings.open": "CmdOrCtrl+.",
            "nav.switch-workspace": "CmdOrCtrl",
          },
        },
      }),
    );
    useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
    await useShortcutsStore.getState().loadPersistedState();
    expect(useShortcutsStore.getState().shortcuts).toEqual({ "settings.open": "CmdOrCtrl+." });
  });

  it("loadPersistedState drops entries for unknown action ids", async () => {
    mockLoadAppState.mockResolvedValue(
      JSON.stringify({
        shortcuts: {
          shortcuts: {
            "settings.open": "CmdOrCtrl+.",
            "bogus.action": "CmdOrCtrl+Shift+Q",
          },
        },
      }),
    );
    useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
    await useShortcutsStore.getState().loadPersistedState();
    expect(useShortcutsStore.getState().shortcuts).toEqual({ "settings.open": "CmdOrCtrl+." });
  });

  it("loadPersistedState flips stateLoaded=true even on parse error", async () => {
    mockLoadAppState.mockResolvedValue("not valid json");
    useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
    await useShortcutsStore.getState().loadPersistedState();
    expect(useShortcutsStore.getState().stateLoaded).toBe(true);
  });

  // T.10 — keybinding migration across the T.6 rename chain.
  describe("migratePersistedShortcuts (pure)", () => {
    it("AC-1: migrates old nav.switch-workspace override to nav.switch-org", () => {
      const { shortcuts, didMigrate } = migratePersistedShortcuts({
        shortcuts: { "nav.switch-workspace": "CmdOrCtrl+Shift+G" },
      });
      expect(didMigrate).toBe(true);
      expect(shortcuts).toEqual({ "nav.switch-org": "CmdOrCtrl+Shift+G" });
    });

    it("AC-4d: migrates old nav.switch-repo override to nav.switch-workspace", () => {
      const { shortcuts, didMigrate } = migratePersistedShortcuts({
        shortcuts: { "nav.switch-repo": "CmdOrCtrl+Shift+G" },
      });
      expect(didMigrate).toBe(true);
      expect(shortcuts).toEqual({ "nav.switch-workspace": "CmdOrCtrl+Shift+G" });
    });

    it("AC-4e: resolves the swap when both old IDs are present", () => {
      const { shortcuts, didMigrate } = migratePersistedShortcuts({
        shortcuts: {
          "nav.switch-workspace": "CmdOrCtrl+Shift+G", // old org switcher
          "nav.switch-repo": "CmdOrCtrl+Shift+H", // old repo switcher
        },
      });
      expect(didMigrate).toBe(true);
      expect(shortcuts).toEqual({
        "nav.switch-org": "CmdOrCtrl+Shift+G",
        "nav.switch-workspace": "CmdOrCtrl+Shift+H",
      });
    });

    it("v2: migrates old viewer.toggle-scratch-note override to viewer.toggle-right-pane", () => {
      const { shortcuts, didMigrate } = migratePersistedShortcuts({
        version: 1,
        shortcuts: { "viewer.toggle-scratch-note": "CmdOrCtrl+J" },
      });
      expect(didMigrate).toBe(true);
      expect(shortcuts).toEqual({ "viewer.toggle-right-pane": "CmdOrCtrl+J" });
    });

    it("AC-2: no renamed IDs present → no-op, didMigrate=false", () => {
      const { shortcuts, didMigrate } = migratePersistedShortcuts({
        shortcuts: { "settings.open": "CmdOrCtrl+." },
      });
      expect(didMigrate).toBe(false);
      expect(shortcuts).toEqual({ "settings.open": "CmdOrCtrl+." });
    });

    it("AC-3: already at current version → skips migration entirely", () => {
      const { shortcuts, didMigrate } = migratePersistedShortcuts({
        version: 2,
        // A post-migration blob may legitimately carry nav.switch-workspace
        // as the *workspace switcher* override — must NOT be re-migrated.
        shortcuts: { "nav.switch-workspace": "CmdOrCtrl+Shift+G" },
      });
      expect(didMigrate).toBe(false);
      expect(shortcuts).toEqual({ "nav.switch-workspace": "CmdOrCtrl+Shift+G" });
    });

    it("does not clobber an explicit existing override on the target ID", () => {
      const { shortcuts } = migratePersistedShortcuts({
        shortcuts: {
          "nav.switch-workspace": "CmdOrCtrl+Shift+G", // → would map to nav.switch-org
          "nav.switch-org": "CmdOrCtrl+Shift+Z", // explicit, must win
        },
      });
      expect(shortcuts["nav.switch-org"]).toBe("CmdOrCtrl+Shift+Z");
      expect(shortcuts["nav.switch-workspace"]).toBeUndefined();
    });

    it("treats a non-numeric version as pre-migration (does not skip on a string)", () => {
      const { didMigrate, shortcuts } = migratePersistedShortcuts({
        // Hand-edited blob: version stored as a string must NOT satisfy the gate.
        version: "1" as unknown as number,
        shortcuts: { "nav.switch-workspace": "CmdOrCtrl+Shift+G" },
      });
      expect(didMigrate).toBe(true);
      expect(shortcuts).toEqual({ "nav.switch-org": "CmdOrCtrl+Shift+G" });
    });

    it("does not throw on a malformed (non-object) shortcuts value", () => {
      expect(() =>
        migratePersistedShortcuts({
          shortcuts: "oops" as unknown as Record<string, string>,
        }),
      ).not.toThrow();
      const { didMigrate } = migratePersistedShortcuts({
        shortcuts: "oops" as unknown as Record<string, string>,
      });
      expect(didMigrate).toBe(false);
    });
  });

  describe("loadPersistedState — T.10 migration", () => {
    it("AC-1: hydrates a pre-version blob and migrates the org-switcher binding", async () => {
      mockLoadAppState.mockResolvedValue(
        JSON.stringify({
          shortcuts: { shortcuts: { "nav.switch-workspace": "CmdOrCtrl+Shift+G" } },
        }),
      );
      useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
      await useShortcutsStore.getState().loadPersistedState();
      expect(useShortcutsStore.getState().shortcuts).toEqual({
        "nav.switch-org": "CmdOrCtrl+Shift+G",
      });
    });

    it("AC-4f: persists the migrated result with the bumped version", async () => {
      vi.useFakeTimers();
      // Old org-switcher override (`nav.switch-workspace` pre-T.6) → migrates to
      // the live `nav.switch-org`. (The `nav.switch-repo → nav.switch-workspace`
      // rename still runs in migratePersistedShortcuts — see AC-4d — but its
      // target was retired in SW-EW-33, so a migrated value there is filtered
      // out on load; this test uses the surviving rename target.)
      mockLoadAppState.mockResolvedValue(
        JSON.stringify({
          shortcuts: { shortcuts: { "nav.switch-workspace": "CmdOrCtrl+Shift+G" } },
        }),
      );
      useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
      await useShortcutsStore.getState().loadPersistedState();
      await vi.advanceTimersByTimeAsync(500); // past the 300ms debounce

      expect(mockSaveAppState).toHaveBeenCalledTimes(1);
      const written = JSON.parse(mockSaveAppState.mock.calls[0][0]);
      expect(written.shortcuts).toEqual({
        version: 2,
        shortcuts: { "nav.switch-org": "CmdOrCtrl+Shift+G", "nav.switch-workspace": null },
      });
    });

    it("AC-3: already-migrated blob does not re-migrate and does not re-persist", async () => {
      vi.useFakeTimers();
      mockLoadAppState.mockResolvedValue(
        JSON.stringify({
          shortcuts: { version: 1, shortcuts: { "nav.switch-org": "CmdOrCtrl+Shift+G" } },
        }),
      );
      useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
      await useShortcutsStore.getState().loadPersistedState();
      await vi.advanceTimersByTimeAsync(500);

      expect(useShortcutsStore.getState().shortcuts).toEqual({
        "nav.switch-org": "CmdOrCtrl+Shift+G",
      });
      expect(mockSaveAppState).not.toHaveBeenCalled();
    });

    it("AC-4g: a truly-unknown id is still dropped through migration", async () => {
      mockLoadAppState.mockResolvedValue(
        JSON.stringify({ shortcuts: { shortcuts: { "bogus.action": "CmdOrCtrl+Shift+Q" } } }),
      );
      useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
      await useShortcutsStore.getState().loadPersistedState();
      expect(useShortcutsStore.getState().shortcuts).toEqual({});
    });
  });

  it("persistState does not write before loadPersistedState completes", () => {
    vi.useFakeTimers();
    useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
    useShortcutsStore.getState().persistState();
    vi.advanceTimersByTime(1000);
    expect(mockSaveAppState).not.toHaveBeenCalled();
  });

  it("persistState writes shortcuts into the persisted app state", async () => {
    vi.useFakeTimers();
    mockLoadAppState.mockResolvedValue(JSON.stringify({ viewer: { stateLoaded: true } }));
    useShortcutsStore.getState().restoreShortcuts({ "settings.open": "CmdOrCtrl+Shift+." });

    useShortcutsStore.getState().persistState();
    await vi.advanceTimersByTimeAsync(500); // past the 300ms debounce

    expect(mockSaveAppState).toHaveBeenCalledTimes(1);
    const written = JSON.parse(mockSaveAppState.mock.calls[0][0]);
    expect(written.shortcuts).toEqual({
      version: 2,
      shortcuts: { "settings.open": "CmdOrCtrl+Shift+." },
    });
    // The patch never sends another writer's viewer state back.
    expect(written).not.toHaveProperty("viewer");
  });
});

describe("binding collisions from overrides saved before a new default", () => {
  it("the registry defaults collide with nothing", () => {
    expect(findBindingCollisions({})).toEqual([]);
  });

  it("finds an override that now shares a key with another action's default", () => {
    // Someone rebound Toggle Terminal to ⌘⇧T before ⌘⇧T became Reopen Closed Tab.
    expect(findBindingCollisions({ "viewer.toggle-terminal": "CmdOrCtrl+Shift+T" })).toEqual([
      {
        keystroke: "CmdOrCtrl+Shift+T",
        actionIds: ["viewer.toggle-terminal", "viewer.reopen-closed-tab"],
      },
    ]);
  });

  it("warns once per collision when overrides load", async () => {
    mockWarn.mockClear();
    mockLoadAppState.mockResolvedValue(
      JSON.stringify({
        shortcuts: { version: 2, shortcuts: { "viewer.toggle-terminal": "CmdOrCtrl+Shift+T" } },
      }),
    );
    useShortcutsStore.setState({ shortcuts: {}, stateLoaded: false });
    await useShortcutsStore.getState().loadPersistedState();
    const warnings = mockWarn.mock.calls.filter(([, msg]) => String(msg).includes("bound to both"));
    expect(warnings).toHaveLength(1);
    expect(String(warnings[0][1])).toContain("viewer.reopen-closed-tab");
  });
});
