// SPDX-License-Identifier: MIT
// SW-AGR-9: the wake event and read-model contract the desktop emits, read
// from the Rust golden fixtures so the two sides cannot drift.
import { describe, expect, it } from "vitest";
import { readFileSync } from "fs";
import { resolve } from "path";

import {
  type AgentRoutineRow,
  WAKE_HISTORY_CAP,
  wakeAnnotation,
  wakeStatusLabel,
  type QueuedWake,
  type WakeDeliveredEvent,
  type WakeHistoryEntry,
  type WakeOutcomeEvent,
} from "./wake-events";

const fixture = <T>(name: string): T =>
  JSON.parse(
    readFileSync(resolve(__dirname, "../../../src-tauri/src/fixtures/wake", name), "utf-8"),
  ) as T;

const events = fixture<{
  "wake-delivered": WakeDeliveredEvent[];
  "wake-outcome": WakeOutcomeEvent[];
  wakeHistory: WakeHistoryEntry[];
}>("wake-events.json");

describe("wake events (golden)", () => {
  it("a delivered wake names its marker for the annotation and the caption", () => {
    const [delivered] = events["wake-delivered"];
    expect(wakeAnnotation(delivered)).toBe("⏰ Triage");
    expect(delivered.caption).toBe("Woken by Triage");
    expect(delivered.origin).toEqual({
      kind: "routine",
      routineKey: "acme/core/barb/triage",
      runId: "run-42",
    });
  });

  it("outcomes carry their status flat, and unknown never offers retry", () => {
    const statuses = events["wake-outcome"].map((event) => event.status);
    expect(statuses).toEqual(["queued", "delivered", "undelivered", "undelivered"]);
    for (const event of events["wake-outcome"]) {
      expect(event.mayRetry).toBe(event.status === "undelivered" && event.origin.kind !== "timer");
      if (event.status === "undelivered") expect(event.reason).toMatch(/^[a-z_]+$/);
      expect(event.replayed).toBe(false);
      expect(["reply", "push", "sync", "local"]).toContain(event.source);
    }
    const offline = events["wake-outcome"][3];
    expect(offline.status === "undelivered" && offline.reason).toBe("machine_offline");
    expect(offline.deliveryId).toBe("");
    expect(wakeStatusLabel({ status: "unknown", detail: "x" })).toContain("Check the terminal");
  });

  it("the tab history holds final deliveries only, within the cap", () => {
    expect(events.wakeHistory.length).toBeLessThanOrEqual(WAKE_HISTORY_CAP);
    expect(events.wakeHistory.every((row) => row.status !== "queued")).toBe(true);
    const timer = events.wakeHistory.find((row) => row.origin.kind === "timer");
    expect(timer?.origin).toEqual({ kind: "timer", timerId: "t-1" });
  });
});

describe("agent_get.routines[].queued (golden)", () => {
  it("is a QueuedWake while a wake waits, null otherwise", () => {
    const { routines } = fixture<{ routines: AgentRoutineRow[] }>("agent-routines-queued.json");
    const byId = (id: string) => routines.find((row) => row.id.endsWith(`/${id}`));
    const queued: QueuedWake | null | undefined = byId("triage")?.queued;
    expect(queued).toMatchObject({ runId: "run-42", sessionId: "s-barb", unconfirmed: false });
    expect(typeof queued?.deadlineMs).toBe("number");
    expect(byId("digest")?.queued).toBeNull();
  });
});
