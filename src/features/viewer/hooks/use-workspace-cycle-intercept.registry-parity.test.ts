// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { readFileSync } from "fs";
import { resolve } from "path";
import { PTY_INTERCEPTED_ACTION_IDS } from "./use-workspace-cycle-intercept";
import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";

// This file uses the REAL registry (no mocks) — it guards the consolidation seam:
// the intercept preventDefault()s a matched chord BEFORE dispatching
// SHORTCUT_ACTIONS.find(id)?.run(). If an intercepted id ever drifts out of the
// registry, the chord would be swallowed from the PTY with no command run (silent
// key loss). It also pins the DRY consolidation: the hook must not re-implement any
// store action inline.

describe("useWorkspaceCycleIntercept — registry parity (OC-7)", () => {
  it("every intercepted id resolves to a real registry action (no silent key loss)", () => {
    const registryIds = new Set(SHORTCUT_ACTIONS.map((a) => a.id));
    const orphans = PTY_INTERCEPTED_ACTION_IDS.filter((id) => !registryIds.has(id));
    expect(orphans).toEqual([]);
  });

  it("every intercepted action id is unique in the list", () => {
    expect(new Set(PTY_INTERCEPTED_ACTION_IDS).size).toBe(PTY_INTERCEPTED_ACTION_IDS.length);
  });

  it("no two intercepted actions share a default keystroke (first-match precedence is unambiguous)", () => {
    const byKeystroke = new Map<string, string>();
    for (const id of PTY_INTERCEPTED_ACTION_IDS) {
      const action = SHORTCUT_ACTIONS.find((a) => a.id === id);
      if (!action) continue; // covered by the orphan test above
      const prev = byKeystroke.get(action.defaultKeystroke);
      expect(prev, `${id} shares ${action.defaultKeystroke} with ${prev ?? ""}`).toBeUndefined();
      byKeystroke.set(action.defaultKeystroke, id);
    }
  });

  it("the hook delegates only — no re-implemented store action remains inline (AC2 DRY)", () => {
    const src = readFileSync(
      resolve(process.cwd(), "src/features/viewer/hooks/use-workspace-cycle-intercept.ts"),
      "utf-8",
    );
    // The consolidation deleted the inline branches; each of these would reintroduce
    // a second behavior home for a command the registry already owns.
    for (const forbidden of [
      "cycleWorkspace",
      "flipLastWorkspace",
      "jumpToTrayIndex",
      "focusAdjacentPane",
    ]) {
      expect(src.includes(forbidden), `hook must not reference ${forbidden}`).toBe(false);
    }
  });
});
