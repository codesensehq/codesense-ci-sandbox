// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { afterEach, expect, it } from "vitest";
import type { HubSnapshot } from "@/features/dashboard/lib/live-hub";
import { resetAgentsStoreForTests, useAgentsStore } from "../stores/agents-store";
import { applyAgentDefinitions } from "./agent-definitions";

// Produced by the Rust owner tests from real service saves and account CAS writes.
function packets(name: string): { before: HubSnapshot; after: HubSnapshot } {
  return JSON.parse(readFileSync(`tests/fixtures/agent-definitions-${name}.json`, "utf8"));
}
afterEach(resetAgentsStoreForTests);

it("retires the store's domain listeners, debounce timers and full-list/detail refresh calls", () => {
  const store = readFileSync("src/features/agents/stores/agents-store.ts", "utf8");
  const bridge = readFileSync("src/features/agents/lib/agent-definitions.ts", "utf8");
  expect(store).not.toMatch(/\blisten\s*\(|\bsetTimeout\s*\(|\bagentsList\s*\(|\bagentDetail\s*\(/);
  expect(bridge).not.toMatch(/\blisten\s*\(|\bsetTimeout\s*\(|\bsetInterval\s*\(/);
});

it("materializes a service save into the existing list and detail without losing an edit", () => {
  const { before, after } = packets("save");
  applyAgentDefinitions(before);
  const key = String(after.snapshot.find((row) => row.rowType === "detail")!.key);
  useAgentsStore.setState({
    selectedKey: key,
    formDirty: true,
    pending: { [`save:${key}`]: true },
  });
  applyAgentDefinitions(after);
  const state = useAgentsStore.getState();
  expect(state.rows!.find((row) => row.key === key)!.name).toBe("Saved without any session");
  expect(state.details[key].definition.name).toBe("Saved without any session");
  expect(state.selectedKey).toBe(key);
  expect(state.formDirty).toBe(true);
  expect(state.pending).toEqual({ [`save:${key}`]: true });
});

it("updates default resolution and eligibility while keeping an explicit binding stable", () => {
  const { before, after } = packets("account");
  applyAgentDefinitions(before);
  const defaults = useAgentsStore
    .getState()
    .rows!.find((row) => row.key.endsWith("/default-live"))!;
  const bound = useAgentsStore.getState().rows!.find((row) => row.key.endsWith("/bound-live"))!;
  expect(defaults.account.spawn_eligible).toBe(true);
  expect(defaults.account.account_id).toBeNull();
  expect(bound.account.spawn_eligible).toBe(false);
  applyAgentDefinitions(after);
  const rows = useAgentsStore.getState().rows!;
  expect(rows.find((row) => row.key === defaults.key)!.account.spawn_eligible).toBe(true);
  expect(rows.find((row) => row.key === defaults.key)!.account.label).toBe("Second");
  expect(rows.find((row) => row.key === bound.key)).toBe(bound);
});
