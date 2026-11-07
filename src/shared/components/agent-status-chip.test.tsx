// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

import { AGENT_STATUSES, AGENT_STATUS_WORDS } from "@/shared/lib/agent-status-config";
import { markOfAgentStatus } from "@/shared/lib/status-mark";

import { AgentStatusChip } from "./agent-status-chip";

describe("AgentStatusChip", () => {
  it("renders each status as its shared mark, its words and a screen-reader aria-label", () => {
    for (const status of AGENT_STATUSES) {
      const { unmount } = render(<AgentStatusChip status={status} />);
      const words = AGENT_STATUS_WORDS[status];
      // The chip is a single labelled indicator (img role), not a live region.
      const chip = screen.getByRole("img", { name: `Status: ${words.toLowerCase()}` });
      expect(chip).toHaveTextContent(words);
      // Its mark is the shared one, decoration beside the words.
      const mark = chip.querySelector("[data-mark]");
      expect(mark).toHaveAttribute("data-mark", markOfAgentStatus(status));
      expect(mark).toHaveAttribute("aria-hidden", "true");
      // The tone is a theme token, never a hex.
      expect(chip.getAttribute("style")).toMatch(/var\(--cs-status-/);
      unmount();
    }
  });

  it("one verb for the generic running base is Working; adds the finer Thinking / Running a tool", () => {
    // the base `running` chip reads "Working"; the finer `thinking`
    // and `tool-running` are the live-signal splits — distinct words, distinct
    // marks, so a tab chip and a nav row can agree on which flavour of work.
    expect(AGENT_STATUS_WORDS.running).toBe("Working");
    expect(AGENT_STATUS_WORDS.thinking).toBe("Thinking");
    expect(AGENT_STATUS_WORDS["tool-running"]).toBe("Running a tool");
  });

  it("SW-WA-E9: a badge `label` overrides the visible text + aria-label, keeping the mark", () => {
    // The Weft Board's PARKED lane reuses the quiet idle ring but must read "Parked".
    render(<AgentStatusChip status="idle" label="Parked" />);
    const chip = screen.getByRole("img", { name: "Status: parked" });
    expect(chip).toHaveTextContent("Parked");
    expect(chip.querySelector("[data-mark]")).toHaveAttribute("data-mark", "ready");
  });

  it("forwards an extra className without dropping its own", () => {
    render(<AgentStatusChip status="running" className="ml-2" />);
    const chip = screen.getByRole("img");
    expect(chip).toHaveClass("ml-2");
    expect(chip).toHaveClass("rounded-full");
  });
});

describe("AgentStatusChip button mode (SW-BA-3 )", () => {
  it("with onClick, renders a <button> whose accessible name carries the target AND the status", () => {
    render(<AgentStatusChip status="running" onClick={() => {}} label="SW-A" />);
    // The aria-label overrides the visible text, so it must include the status too
    // (else a screen-reader user loses the state the pill shows) — review r1.
    const btn = screen.getByRole("button", { name: "Go to SW-A, working" });
    expect(btn).toHaveTextContent("Working");
    expect(btn).toHaveClass("rounded-full");
    expect(btn.querySelector("[data-mark]")).toHaveAttribute("data-mark", "thinking");
    // It is NOT also exposed as an img badge (no double-announce).
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
  });

  it("fires onClick when the button chip is activated", () => {
    const onClick = vi.fn();
    render(<AgentStatusChip status="paused" onClick={onClick} label="SW-B" />);
    fireEvent.click(screen.getByRole("button", { name: "Go to SW-B, needs approval" }));
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("falls back to the status words for the target when no label is given", () => {
    render(<AgentStatusChip status="errored" onClick={() => {}} />);
    expect(screen.getByRole("button", { name: "Go to Error, error" })).toBeInTheDocument();
  });

  it("without onClick, stays a role=img badge (zero regression)", () => {
    render(<AgentStatusChip status="running" />);
    expect(screen.getByRole("img", { name: "Status: working" })).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });
});
