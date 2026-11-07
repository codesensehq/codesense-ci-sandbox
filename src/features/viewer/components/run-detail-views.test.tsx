// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import { render, screen } from "@/test-utils";
import userEvent from "@testing-library/user-event";
import { RunDetailViews } from "./run-detail-views";
import type { WvPendingAssertionDto } from "@/tauri/commands";
const data = vi.hoisted(() => ({ assertions: [] as WvPendingAssertionDto[], evidence: vi.fn() }));
vi.mock("../hooks/use-run-review", () => ({
  useRunReview: () => ({
    snapshot: { snapshot: data.assertions.map((assertion) => ({ assertion })) },
  }),
}));
vi.mock("../hooks/use-run-evidence", () => ({
  useRunEvidence: (...args: unknown[]) => {
    data.evidence(...args);
    return { state: { kind: "unavailable" }, reload: vi.fn() };
  },
}));
vi.mock("@/tauri/commands", () => ({
  wvRatify: vi.fn(),
  wvRevoke: vi.fn(),
  readRunArtifact: vi.fn(),
  readRunTranscript: vi.fn(),
}));
const output = <div>the output body</div>;
beforeEach(() => {
  data.assertions = [];
  data.evidence.mockClear();
});
it("opens on Output and keeps Evidence and Findings lazy", () => {
  render(<RunDetailViews runId="run-1" storyId="SW-A" defaultView="output" output={output} />);
  expect(screen.getAllByRole("tab").map((t) => t.textContent)).toEqual([
    "Output",
    "Findings",
    "Evidence",
  ]);
  expect(screen.getByRole("tab", { name: "Output" })).toHaveAttribute("aria-selected", "true");
  expect(data.evidence.mock.lastCall?.slice(0, 2)).toEqual([null, null]);
  expect(screen.queryByTestId("findings-empty")).not.toBeInTheDocument();
});
it("activates a retained panel by keyboard and announces the reveal without moving focus", async () => {
  const user = userEvent.setup();
  render(<RunDetailViews runId="run-1" storyId="SW-A" defaultView="output" output={output} />);
  screen.getByRole("tab", { name: "Output" }).focus();
  await user.keyboard("{ArrowRight}");
  const findings = screen.getByRole("tab", { name: "Findings" });
  expect(findings).toHaveFocus();
  expect(findings).toHaveAttribute("aria-selected", "true");
  expect(screen.getByText("Findings view")).toHaveAttribute("aria-live", "polite");
  const panel = document.getElementById(findings.getAttribute("aria-controls")!);
  expect(panel).toHaveAttribute("tabindex", "0");
  expect(data.evidence.mock.lastCall?.slice(0, 2)).toEqual(["SW-A", "run-1"]);
  await user.keyboard("{ArrowLeft}");
  expect(panel).toHaveAttribute("hidden");
  expect(screen.getByRole("tab", { name: "Output" })).toHaveFocus();
});
it("keeps an empty Endorse tab outside the tab order", () => {
  render(<RunDetailViews runId="run-1" storyId="SW-A" defaultView="output" output={output} />);
  const endorse = screen.getAllByRole("tab", { hidden: true }).at(-1)!;
  expect(endorse).toHaveAttribute("aria-hidden", "true");
  expect(endorse).toHaveAttribute("tabindex", "-1");
});
it("derives the badge and lazy Endorse body from the same pending rows", async () => {
  data.assertions = [
    {
      id: 1,
      runId: "run-1",
      sessionId: "worker",
      claim: "Ready",
      channel: "review",
      postedAt: "2026-10-21",
      disposition: "pending",
    },
  ];
  const user = userEvent.setup();
  render(<RunDetailViews runId="run-1" storyId="SW-A" defaultView="output" output={output} />);
  expect(screen.queryByTestId("endorsement-panel")).not.toBeInTheDocument();
  await user.click(screen.getByRole("tab", { name: "Endorse (1)" }));
  expect(screen.getByTestId("endorsement-panel")).toBeVisible();
  expect(screen.getByText("Ready")).toBeInTheDocument();
});
