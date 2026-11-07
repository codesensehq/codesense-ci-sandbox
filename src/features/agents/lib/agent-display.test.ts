// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import adapterGolden from "../../../../src-tauri/src/commands/agents_launch/adapter_launch_golden.json";
import { hermesGolden, hermesGenerated, hermesAdmission } from "@/app/mock/handlers/hermes-golden";
import nativePermissions from "./opencode-permissions.fixture.json";

import { accountLine, accountReasonCopy, agentSentence } from "./agent-display";

function opencodeDetail(content: Record<string, unknown>, own: Record<string, string>) {
  return {
    definition: {
      name: "OC",
      runtime: "opencode",
      args: [],
      mcp: { servers: [] },
      permissions: { opencode: { permission: own } },
    },
    resolved: {
      account: { spawn_eligible: true, source: "default" },
      manifest: {
        generated: [{ id: "opencode-launch-permissions", content, required: [] }],
        entries: [],
        totals: { files: 0 },
        errors: [],
      },
    },
  } as never;
}

describe("agentSentence permissions for OpenCode", () => {
  it.each(["deny", "ask"] as const)("reads the captured generated %s fallback", (effect) => {
    expect(
      agentSentence(opencodeDetail(nativePermissions[effect], { "*": effect, read: "allow" }))
        .permissions,
    ).toBe(`2 custom permission rules · unspecified tools ${effect}`);
  });

  it.each(["deny", "ask", "allow"])(
    "reads the generated %s default with no custom rules",
    (effect) => {
      const content = {
        permissions: [
          { action: "*", resource: "*", effect },
          { action: "read", resource: "*", effect: "allow" },
        ],
      };
      expect(agentSentence(opencodeDetail(content, {})).permissions).toBe(
        `0 custom permission rules · unspecified tools ${effect}`,
      );
    },
  );

  it.each([false, true])(
    "ignores specific rules around the catch-all (specific first: %s)",
    (specificFirst) => {
      const fallback = { action: "*", resource: "*", effect: "deny" };
      const specific = [
        { action: "read", resource: "*", effect: "allow" },
        { action: "shell", resource: "*", effect: "ask" },
      ];
      const content = {
        permissions: specificFirst ? [...specific, fallback] : [fallback, ...specific],
      };
      const before = structuredClone(content);
      expect(
        agentSentence(opencodeDetail(content, { "*": "deny", read: "allow", bash: "ask" }))
          .permissions,
      ).toBe("3 custom permission rules · unspecified tools deny");
      expect(content).toEqual(before);
    },
  );

  it("falls back to ask without a wildcard", () => {
    expect(agentSentence(opencodeDetail({ permissions: [] }, {})).permissions).toContain(
      "unspecified tools ask",
    );
  });
});

describe("accountLine names the resolved account", () => {
  const choice = (over: Record<string, unknown>) =>
    ({
      source: "runtime_default",
      bound_account_id: null,
      resolved_source: "system_default",
      account_id: null,
      label: null,
      reason: "optional-system-default",
      spawn_eligible: true,
      ...over,
    }) as never;

  it("marks the Active account", () => {
    const line = accountLine(
      choice({ resolved_source: "active", label: "Work", reason: "active-selection" }),
      "claude",
    );
    expect(line).toEqual({ kind: "ok", text: "Work (Active)" });
  });

  it("shows a pin by its label", () => {
    const line = accountLine(
      choice({ source: "binding", resolved_source: "explicit", label: "Home" }),
      "claude",
    );
    expect(line).toEqual({ kind: "ok", text: "Home" });
  });

  it("names the own login only for a real system default", () => {
    expect(accountLine(choice({}), "codex")).toEqual({
      kind: "ok",
      text: "your own Codex login",
    });
  });

  it("warns, naming the missing account", () => {
    const line = accountLine(
      choice({ resolved_source: "active", spawn_eligible: false, reason: "account-missing" }),
      "claude",
    );
    expect(line).toEqual({ kind: "warning", text: "Can't launch: the account no longer exists" });
    expect(accountReasonCopy("generation-mismatch")).toContain("changed after");
  });
});

describe("Hermes Overview and mock read native per-mode golden facts", () => {
  it.each(
    Object.entries(adapterGolden).filter(
      ([key, row]) => key.includes("hermes") && "generated" in row,
    ),
  )("%s agrees with the generated approvals, toolsets and scoped denies", (_key, raw) => {
    const row = raw as unknown as {
      definition: import("@/tauri/commands").AgentDefinition;
      generated: import("@/tauri/commands").AgentGeneratedEntry[];
      launch: { args: string[] };
      launch_admission: import("@/tauri/commands").AgentLaunchAdmission;
    };
    const expected = row.launch_admission.permission_display;
    expect(expected).toBeTruthy();
    expect(
      agentSentence({
        definition: row.definition,
        resolved: {
          launch_admission: row.launch_admission,
          manifest: { generated: row.generated, entries: [], totals: { files: 0 }, errors: [] },
        },
      } as never).permissions,
    ).toBe(expected);
  });

  it.each([
    ["hermes-native-reset Interactive", "ask approvals · 2 toolsets · 1 tool deny rules"],
    ["hermes-native-alias Interactive", "yolo approvals · 2 toolsets · 1 tool deny rules"],
    [
      "hermes-native-narrow Interactive",
      "ask approvals · 1 selected toolset: web (memory, skill_manage, cronjob_manage blocked) · 1 tool deny rules",
    ],
    [
      "hermes-native-wide Interactive",
      "ask approvals · toolsets all (memory, skill_manage, cronjob_manage blocked) · 1 tool deny rules",
    ],
  ])("%s preserves the native resolver's explicit effective fact", (key, expected) => {
    const row = (adapterGolden as Record<string, unknown>)[key] as {
      definition: import("@/tauri/commands").AgentDefinition;
      launch_admission: import("@/tauri/commands").AgentLaunchAdmission;
    };
    expect(row.launch_admission.permission_display).toBe(expected);
    expect(
      agentSentence({
        definition: row.definition,
        resolved: {
          launch_admission: row.launch_admission,
          manifest: { generated: [], entries: [], totals: { files: 0 }, errors: [] },
        },
      } as never).permissions,
    ).toBe(expected);
  });

  it.each([false, true])(
    "uses the recorded foreground/headless mock (unattended: %s)",
    (unattended) => {
      for (const headless of [false, true]) {
        const row = hermesGolden(unattended, false, headless);
        const key = `builtin/hermes${unattended ? "-unattended" : ""} ${headless ? "Headless" : "Interactive"}`;
        expect(row).toEqual((adapterGolden as Record<string, unknown>)[key]);
        expect(hermesGenerated(unattended, headless)).toEqual(row.generated);
        expect(
          hermesAdmission({ ...row.definition, launch: headless ? "headless" : "foreground" })
            .permission_display,
        ).toBe(row.launch_admission.permission_display);
        expect(
          hermesAdmission({
            ...row.definition,
            launch: headless ? "headless" : "foreground",
            startup_settings: {
              model: { provider: "custom", base_url: "http://127.0.0.1:9000/v1", default: "local" },
            },
          }).permission_display,
        ).toBe(row.launch_admission.permission_display);
      }
      if (!unattended)
        expect(hermesGenerated(false, true)).not.toEqual(hermesGenerated(false, false));
    },
  );
});
