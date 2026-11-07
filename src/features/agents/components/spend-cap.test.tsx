// SPDX-License-Identifier: MIT
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({
  set: vi.fn(),
  accept: vi.fn(),
  rateAccept: vi.fn(),
  watch: new Map<string, (value: any) => void>(),
}));
vi.mock("@/tauri/commands", async (load) => ({
  ...(await load<typeof import("@/tauri/commands")>()),
  spendCapSet: mocks.set,
  spendCapAccept: mocks.accept,
  spendRateAccept: mocks.rateAccept,
}));
vi.mock("@/features/dashboard/lib/live-hub", () => ({
  liveHub: {
    watch: (topic: string, _: string, fn: (value: any) => void) => {
      mocks.watch.set(topic, fn);
      return () => mocks.watch.delete(topic);
    },
  },
}));
import { RoutineSpendCap, SpendCapBlock, SpendCapChip, USAGE_NOT_RECORDED } from "./spend-cap";
import {
  useAgentListProjectionStore,
  watchAgentList,
} from "@/features/viewer/lib/agent-list-projection";
import { useAgentsStore } from "../stores/agents-store";
import { showAgentCap } from "./agents-tab-nav";
import { useRoutinesStore } from "@/features/routines/stores/routines-store";
import type { SpendCapView } from "@/tauri/commands";
const key = "org/team/worker";
const base: SpendCapView = {
  today: 12,
  week: 80,
  cap: { perDay: 50, perWeek: 200 },
  state: "ok",
  pending: false,
  requestedCap: { perDay: 50, perWeek: 200 },
  unpricedModels: [],
};
function seed(value: SpendCapView = base) {
  useAgentListProjectionStore.setState({ spend: { [`agent:${key}`]: value } });
}
beforeEach(() => {
  vi.clearAllMocks();
  mocks.set.mockResolvedValue(undefined);
  mocks.accept.mockResolvedValue(undefined);
  useAgentListProjectionStore.setState({ spend: {} });
});
afterEach(cleanup);
it("shows no row chip without a cap and offers Set a cap in detail", () => {
  seed({ ...base, cap: null, requestedCap: null });
  render(
    <>
      <SpendCapChip agentKey={key} />
      <SpendCapBlock scopeKind="agent" scopeKey={key} />
    </>,
  );
  expect(screen.queryByTestId("spend-cap-chip")).toBeNull();
  expect(screen.getByText("No cap · $12 today")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Set a cap" }));
  expect(screen.getByLabelText("Per day")).toBeTruthy();
});
it.each([
  ["ok", "$80 of $200 this week"],
  ["warn", "$80 of $200 this week"],
  ["reached", "Cap reached · $80 this week"],
  ["unpriced", "Can't price gpt-x"],
  ["unrecorded", "Usage not recorded"],
] as const)("renders %s with the closer window and accessible tones", (state, text) => {
  seed({ ...base, state, unpricedModels: state === "unpriced" ? ["gpt-x"] : [] });
  render(<SpendCapChip agentKey={key} />);
  const chip = screen.getByTestId("spend-cap-chip");
  expect(chip.textContent).toBe(text);
  if (state === "reached") expect(chip.getAttribute("role")).toBe("status");
  if (state === "warn" || state === "unpriced" || state === "unrecorded") {
    expect(chip.className).not.toMatch(/rounded|\bbg-/);
    expect(chip.className).toContain("text-warning-ink");
    expect(chip.className).not.toContain("amber");
  }
  if (state === "unrecorded") expect(chip.getAttribute("title")).toBe(USAGE_NOT_RECORDED);
});
it("saves a person's new cap through the native set command", async () => {
  seed();
  render(<SpendCapBlock scopeKind="agent" scopeKey={key} />);
  fireEvent.change(screen.getByLabelText("Per day"), { target: { value: "75" } });
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() =>
    expect(mocks.set).toHaveBeenCalledWith("agent", key, { perDay: 75, perWeek: 200 }),
  );
});
it("shows pending and accepts only the exact displayed request", async () => {
  seed({ ...base, pending: true, requestedCap: { perDay: 500, perWeek: 200 } });
  render(<SpendCapBlock scopeKind="agent" scopeKey={key} />);
  expect(screen.getByText(/asked to raise/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Accept" }));
  await waitFor(() =>
    expect(mocks.accept).toHaveBeenCalledWith("agent", key, { perDay: 500, perWeek: 200 }),
  );
});
it("Keep resolves only the reviewed request while retaining the effective cap", async () => {
  seed({ ...base, pending: true, requestedCap: { perDay: 500, perWeek: 200 } });
  render(<SpendCapBlock scopeKind="agent" scopeKey={key} />);
  fireEvent.click(screen.getByRole("button", { name: "Keep $50" }));
  await waitFor(() =>
    expect(mocks.accept).toHaveBeenCalledWith("agent", key, { perDay: 500, perWeek: 200 }, true),
  );
  expect(mocks.set).not.toHaveBeenCalled();
});
it("updates the mounted row from the shared live delta without reloading", () => {
  const stop = watchAgentList();
  const push = (spend: SpendCapView) =>
    mocks.watch.get("agentList")!({
      snapshot: [{ id: `spend:agent:${key}`, rowType: "spend", scopeKind: "agent", key, spend }],
    });
  act(() => push(base));
  render(<SpendCapChip agentKey={key} />);
  act(() => push({ ...base, today: 50, state: "reached" }));
  expect(screen.getByRole("status").textContent).toBe("Cap reached · $50 today");
  stop();
});

it.each(["wake", "command"])("shows the agent-cap note for %s routines", (mode) => {
  render(
    <RoutineSpendCap
      row={
        { def: { action: mode === "command" ? "command" : "agent" }, effective_mode: mode } as any
      }
    />,
  );
  expect(screen.getByText("Spend counts toward the agent's cap")).toBeTruthy();
  expect(screen.queryByLabelText("Per day")).toBeNull();
});
it("shows a fresh routine's own live values and native controls", async () => {
  useAgentListProjectionStore.setState({ spend: { [`routine:${key}`]: base } });
  render(
    <RoutineSpendCap row={{ key, def: { action: "agent" }, effective_mode: "fresh" } as any} />,
  );
  expect(screen.getByText("Today $12 of $50 · This week $80 of $200")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(mocks.set).toHaveBeenCalledWith("routine", key, base.cap));
});
it.each([
  ["agent detail", "agent", <SpendCapBlock scopeKind="agent" scopeKey={key} />],
  ["routine detail", "routine", <SpendCapBlock scopeKind="routine" scopeKey={key} />],
  [
    "routine page",
    "routine",
    <RoutineSpendCap row={{ key, def: { action: "agent" }, effective_mode: "fresh" } as any} />,
  ],
] as const)("says why spend is unrecorded on %s", (_, scopeKind, ui) => {
  useAgentListProjectionStore.setState({
    spend: { [`${scopeKind}:${key}`]: { ...base, today: 0, state: "unrecorded" } },
  });
  render(ui);
  expect(screen.getByText("Usage not recorded").getAttribute("title")).toBe(USAGE_NOT_RECORDED);
});
it("keeps a native error visible without losing typed values", async () => {
  seed();
  mocks.set.mockRejectedValue(new Error("Changed elsewhere — review it again"));
  render(<SpendCapBlock scopeKind="agent" scopeKey={key} />);
  fireEvent.change(screen.getByLabelText("Per day"), { target: { value: "75" } });
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("Changed elsewhere"));
  expect((screen.getByLabelText("Per day") as HTMLInputElement).value).toBe("75");
});

it("accepts removal only with the exact null request", async () => {
  seed({ ...base, pending: true, requestedCap: null });
  render(<SpendCapBlock scopeKind="agent" scopeKey={key} />);
  expect(screen.getByText(/remove the daily cap/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Accept" }));
  await waitFor(() => expect(mocks.accept).toHaveBeenCalledWith("agent", key, null));
});
it("focuses the cap block when opened by a Needs-you destination", () => {
  seed();
  useAgentsStore.setState({ capFocusKey: key });
  render(<SpendCapBlock scopeKind="agent" scopeKey={key} />);
  expect(document.activeElement).toBe(screen.getByRole("region", { name: "Spend cap" }));
});

it.each(["agent", "routine"] as const)(
  "reviews exact pending %s model prices through the native command",
  async (scopeKind) => {
    const request = {
      modelPrefix: "unknown-model",
      effectiveFrom: "2026-10-22",
      inputPerMtok: 1.2345,
      outputPerMtok: 7.5,
      cacheReadPerMtok: 0.12345,
      long: { fromTokens: 200000, inputPerMtok: 3, outputPerMtok: 12, cacheReadPerMtok: 0.3 },
    };
    useAgentListProjectionStore.setState({
      spend: {
        [`${scopeKind}:${key}`]: {
          ...base,
          state: "unpriced",
          unpricedModels: ["unknown-model"],
          pendingRates: [request],
        },
      },
    });
    render(<SpendCapBlock scopeKind={scopeKind} scopeKey={key} />);
    expect(screen.getByText(/input \$1.2345, output \$7.5, cache read \$0.12345/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Accept rate for unknown-model" }));
    await waitFor(() => expect(mocks.rateAccept).toHaveBeenCalledWith(request));
    expect(mocks.accept).not.toHaveBeenCalled();
    expect(mocks.set).not.toHaveBeenCalled();
  },
);

it("opens a routine rate notice at that routine's cap block", () => {
  showAgentCap(key, "org/team/daily");
  expect(useAgentsStore.getState().tabView).toBe("schedule");
  expect(useAgentsStore.getState().capFocusKey).toBe("routine:org/team/daily");
  expect(useRoutinesStore.getState().selectedKey).toBe("org/team/daily");
});
