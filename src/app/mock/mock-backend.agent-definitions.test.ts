// SPDX-License-Identifier: MIT
import { afterEach, expect, it } from "vitest";
import type { HubSnapshot } from "@/features/dashboard/lib/live-hub";
import { createMockBackend } from "./mock-backend";
import { MockEventBus } from "./mock-events";

afterEach(() => {
  delete (globalThis as { __mockAgentContext?: unknown }).__mockAgentContext;
  delete (globalThis as { __mockAgents?: unknown }).__mockAgents;
});

it("keeps the opt-in context demo's agents in its live catalog after a teammate edit", async () => {
  const events = new MockEventBus();
  const invoke = createMockBackend(undefined, "?context=demo", events);
  const before = (await invoke("agent_definitions_prepare", { visible: true })) as HubSnapshot;
  const barb = before.snapshot.find(
    (row) => (row.row as { name?: string } | undefined)?.name === "Barb",
  )!;
  expect(barb).toBeDefined();
  const deltas: unknown[] = [];
  events.observe("codesense:delta", (delta) => deltas.push(delta));
  const world = (globalThis as unknown as { __mockAgentContext: { editAs: (key: string) => void } })
    .__mockAgentContext;
  world.editAs(String(barb.key));
  const after = (await invoke("agent_definitions_prepare", { visible: true })) as HubSnapshot;
  expect(after.snapshot.find((row) => row.id === barb.id)!.row).toMatchObject({ revision: 4 });
  expect(after.rev).toBeGreaterThan(before.rev);
  expect(deltas).toHaveLength(1);
});
