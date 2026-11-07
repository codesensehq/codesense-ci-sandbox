// SPDX-License-Identifier: MIT
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { LiveRequests } from "./live-requests";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const waiting = {
  slug: "ai-usage",
  title: "AI usage",
  by: "agent",
  requestedAt: "2026-11-03T10:00:00Z",
  liveOn: false,
  waiting: true,
  terminals: true,
  control: false,
  widerAudience: false,
  viewersToAdd: 0,
  declined: false,
  shown: { sha: "abc", audience: "private", shareAdd: ["a@b.co"] },
};

beforeEach(() => {
  vi.mocked(invoke).mockReset();
});

it("says live data is paused and confirms only on the person's click", async () => {
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === "dashboard_live_requests" ? [waiting] : {},
  );
  render(<LiveRequests />);
  expect(await screen.findByText("Live data is paused until you confirm")).toBeTruthy();
  expect(invoke).not.toHaveBeenCalledWith("dashboard_live_confirm", expect.anything());
  await userEvent.click(screen.getByRole("button", { name: "Turn on live data" }));
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("dashboard_live_confirm", {
      slug: "ai-usage",
      shown: { sha: "abc", audience: "private", shareAdd: ["a@b.co"] },
    }),
  );
});

it("declines a request and turns a live dashboard off", async () => {
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === "dashboard_live_requests"
      ? [waiting, { ...waiting, slug: "on-air", waiting: false, liveOn: true }]
      : undefined,
  );
  render(<LiveRequests />);
  await userEvent.click(await screen.findByRole("button", { name: "Decline" }));
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("dashboard_live_decline", { slug: "ai-usage" }),
  );
  await userEvent.click(screen.getByRole("button", { name: "Turn off live data" }));
  await waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("dashboard_live_turn_off", { slug: "on-air" }),
  );
});

it("renders nothing when no dashboard is live or waiting", async () => {
  vi.mocked(invoke).mockResolvedValue([]);
  const { container } = render(<LiveRequests />);
  await waitFor(() => expect(invoke).toHaveBeenCalled());
  expect(container.textContent).toBe("");
});

it("says team visibility is reserved and widens nothing", async () => {
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === "dashboard_live_requests"
      ? [{ ...waiting, widerAudience: true, shown: { ...waiting.shown, audience: "team" } }]
      : {},
  );
  render(<LiveRequests />);
  const ask = await screen.findByText(/asked to turn on live data that/);
  expect(ask.textContent).toContain(
    "seen by only you (team visibility is reserved, currently the same as private)",
  );
  expect(ask.textContent).toContain(
    "sets visibility to team (reserved, currently the same as private: no one new can see it)",
  );
  expect(ask.textContent).not.toContain("your team");
});
