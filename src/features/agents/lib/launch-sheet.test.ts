// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  agentIdOfKey,
  defaultAgentKey,
  launchHandle,
  suggestName,
  toLaunchArgs,
  validateLaunch,
  type LaunchSheetForm,
} from "./launch-sheet";
import type { AgentRow } from "@/tauri/commands";

const form = (patch: Partial<LaunchSheetForm> = {}): LaunchSheetForm => ({
  agentKey: "acme/backend/triage",
  manager: null,
  where: "story",
  storyId: "BUG-151",
  name: "Replay checker",
  queueRole: undefined,
  face: null,
  task: "",
  ...patch,
});

describe("launch sheet rules", () => {
  it("needs an agent, a story for a story launch, a usable name", () => {
    expect(validateLaunch(form())).toEqual({});
    expect(Object.keys(validateLaunch(form({ agentKey: "" })))).toEqual(["agentKey"]);
    expect(Object.keys(validateLaunch(form({ storyId: "  " })))).toEqual(["storyId"]);
    // A team folder needs no story.
    expect(validateLaunch(form({ where: "team-workspace", storyId: "" }))).toEqual({});
    expect(validateLaunch(form({ name: "" })).name).toMatch(/name/);
    expect(validateLaunch(form({ name: "!!!" })).name).toMatch(/letter or digit/);
    expect(validateLaunch(form({ name: "x".repeat(65) })).name).toMatch(/64/);
  });

  it("turns the form into the launch command's args, leaving blanks out", () => {
    expect(
      toLaunchArgs(
        form({
          manager: "BUG-151#triage",
          queueRole: "qa",
          face: "builtin-funny:robot",
          task: "Check the replay.",
          name: "  Replay checker ",
        }),
      ),
    ).toEqual({
      target: "story",
      storyId: "BUG-151",
      name: "Replay checker",
      agent: "acme/backend/triage",
      initialPrompt: "Check the replay.",
      launchedBy: "BUG-151#triage",
      queueRole: "qa",
      avatar: "builtin-funny:robot",
    });
    expect(toLaunchArgs(form({ where: "team-workspace", task: "   " }))).toEqual({
      target: "team-workspace",
      name: "Replay checker",
      agent: "acme/backend/triage",
    });
    expect(toLaunchArgs(form({ queueRole: null }))).toHaveProperty("queueRole", null);
    expect(toLaunchArgs(form())).not.toHaveProperty("queueRole");
    expect(toLaunchArgs(form({ queueRole: "reviewer" }))).toHaveProperty("queueRole", "reviewer");
  });

  it("names a launch after its agent, numbering past the names already running", () => {
    expect(suggestName("Triage", [])).toBe("Triage");
    expect(suggestName("Triage", ["triage", "Triage 2"])).toBe("Triage 3");
    expect(suggestName("", [])).toBe("Agent");
  });

  it("predicts the handle the backend gives a launch", () => {
    expect(launchHandle(form({ name: "Replay  Checker!" }))).toBe("BUG-151#replay-checker");
    expect(launchHandle(form({ where: "team-workspace", name: "Sprint 1" }))).toBe("sprint-1");
  });

  it("reads an agent's id from its key and defaults to the built-in Claude", () => {
    expect(agentIdOfKey("acme/backend/triage")).toBe("triage");
    expect(agentIdOfKey("builtin/claude")).toBe("claude");
    const row = (key: string, archived = false) => ({ key, archived }) as AgentRow;
    expect(defaultAgentKey([row("acme/backend/triage"), row("builtin/claude")])).toBe(
      "builtin/claude",
    );
    expect(defaultAgentKey([row("x/y/z", true), row("acme/backend/triage")])).toBe(
      "acme/backend/triage",
    );
    expect(defaultAgentKey([])).toBe("");
  });
});

describe("launch sheet existing field contract", () => {
  it.each(["story", "team-workspace"] as const)(
    "preserves every non-destination field for %s",
    (where) => {
      const value = form({
        where,
        storyId: " BUG-151 ",
        agentKey: "acme/backend/triage",
        manager: "BUG-151#manager",
        name: "  Replay checker  ",
        role: "investigator",
        queueRole: "qa",
        face: "builtin-funny:robot",
        task: "  /resume\nKeep this spacing.  ",
      });
      expect(validateLaunch(value)).toEqual({});
      expect(toLaunchArgs(value, "box")).toEqual({
        target: where,
        ...(where === "story" ? { storyId: "BUG-151", machineId: "box" } : {}),
        name: "Replay checker",
        agent: "acme/backend/triage",
        initialPrompt: "  /resume\nKeep this spacing.  ",
        launchedBy: "BUG-151#manager",
        role: "investigator",
        queueRole: "qa",
        avatar: "builtin-funny:robot",
      });
      expect(validateLaunch({ ...value, name: "" }).name).toBe(
        "Give it a name: it becomes the tab's name.",
      );
      expect(validateLaunch({ ...value, name: "!!!" }).name).toBe(
        "Use at least one letter or digit.",
      );
      expect(validateLaunch({ ...value, name: "a".repeat(65) }).name).toBe(
        "Keep it to 64 characters.",
      );
      expect(validateLaunch({ ...value, agentKey: "" }).agentKey).toBe("Pick the agent to launch.");
    },
  );
  it("freezes the serialized story launch arguments, including property order and prompt whitespace", () => {
    expect(
      JSON.stringify(
        toLaunchArgs(
          form({
            storyId: " BUG-151 ",
            manager: "BUG-151#manager",
            face: "builtin-funny:robot",
            task: "  /resume\nContinue.  ",
          }),
          "box",
        ),
      ),
    ).toBe(
      '{"target":"story","storyId":"BUG-151","machineId":"box","name":"Replay checker","agent":"acme/backend/triage","initialPrompt":"  /resume\\nContinue.  ","launchedBy":"BUG-151#manager","avatar":"builtin-funny:robot"}',
    );
  });
});
