// SPDX-License-Identifier: MIT
import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CONTEXT_READY_FLASH_MS, ContextLandingChip } from "./context-landing-chip";

describe("ContextLandingChip", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("says context is landing while the wave runs", () => {
    render(<ContextLandingChip landing landed={false} failed={false} timedOut={false} />);
    expect(screen.getByTestId("context-landing-chip")).toHaveTextContent("context landing…");
  });

  it("flips to context ready for a few seconds, then goes away", () => {
    const { rerender } = render(
      <ContextLandingChip landing landed={false} failed={false} timedOut={false} />,
    );
    rerender(<ContextLandingChip landing={false} landed failed={false} timedOut={false} />);
    expect(screen.getByTestId("context-landing-chip")).toHaveTextContent("context ready");
    act(() => vi.advanceTimersByTime(CONTEXT_READY_FLASH_MS));
    expect(screen.queryByTestId("context-landing-chip")).toBeNull();
  });

  it("shows nothing on a terminal that never saw context land (reopen, root shell)", () => {
    render(<ContextLandingChip landing={false} landed failed={false} timedOut={false} />);
    expect(screen.queryByTestId("context-landing-chip")).toBeNull();
  });

  it("warns when a held command ran before context was ready", () => {
    render(<ContextLandingChip landing landed={false} failed={false} timedOut />);
    const chip = screen.getByTestId("context-landing-chip");
    expect(chip).toHaveTextContent("context not ready");
    expect(chip).toHaveAttribute("role", "status");
  });

  it("warns when context failed to land", () => {
    const { rerender } = render(
      <ContextLandingChip landing landed={false} failed={false} timedOut={false} />,
    );
    rerender(<ContextLandingChip landing={false} landed={false} failed timedOut={false} />);
    expect(screen.getByTestId("context-landing-chip")).toHaveTextContent("context not ready");
  });

  it("says a held command will run once context is ready", () => {
    render(<ContextLandingChip landing landed={false} failed={false} held timedOut={false} />);
    expect(screen.getByTestId("context-landing-chip")).toHaveTextContent(
      /your command runs when it's ready/,
    );
  });

  it("never says ready when landing stopped without the context arriving (watchdog)", () => {
    const { rerender } = render(
      <ContextLandingChip landing landed={false} failed={false} timedOut={false} />,
    );
    rerender(<ContextLandingChip landing={false} landed={false} failed={false} timedOut={false} />);
    expect(screen.getByTestId("context-landing-chip")).toHaveTextContent("context not ready");
  });
  it("stays calm once the context landed, whatever later waves do to the surface", () => {
    // Round-6 north star: the context landed, then a later team edit left the
    // unfocused worktree "out of date" (landed=false, landing=false). The context
    // is still there — the chip must not turn amber.
    const { rerender } = render(
      <ContextLandingChip landing landed={false} failed={false} timedOut={false} />,
    );
    rerender(<ContextLandingChip landing={false} landed failed={false} timedOut={false} />);
    act(() => vi.advanceTimersByTime(CONTEXT_READY_FLASH_MS));
    rerender(<ContextLandingChip landing={false} landed={false} failed={false} timedOut={false} />);
    expect(screen.queryByTestId("context-landing-chip")).toBeNull();
  });

  it("stays calm when a later wave on a landed worktree fails or runs", () => {
    const { rerender } = render(
      <ContextLandingChip landing landed={false} failed={false} timedOut={false} />,
    );
    rerender(<ContextLandingChip landing={false} landed failed={false} timedOut={false} />);
    // A later wave re-syncs the landed worktree: never amber, just the ready flash.
    rerender(<ContextLandingChip landing={false} landed={false} failed={false} timedOut={false} />);
    expect(screen.getByTestId("context-landing-chip")).toHaveAttribute("data-phase", "ready");
    act(() => vi.advanceTimersByTime(CONTEXT_READY_FLASH_MS));
    rerender(<ContextLandingChip landing={false} landed={false} failed timedOut={false} />);
    expect(screen.queryByTestId("context-landing-chip")).toBeNull();
  });
});
