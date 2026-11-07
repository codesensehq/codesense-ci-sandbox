// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { act, render, screen, within } from "@/test-utils";

import { LiveCountBadge, LiveList } from "./live-list";

interface Row {
  id: string;
  name: string;
}

const row = (id: string): Row => ({ id, name: `Item ${id}` });

function renderList(
  props: Partial<React.ComponentProps<typeof LiveList<Row>>> & { items: Row[] | null },
) {
  return render(
    <LiveList<Row>
      getKey={(r) => r.id}
      renderRow={(r) => <span data-testid={`row-${r.id}`}>{r.name}</span>}
      getLabel={(r) => r.name}
      noun={{ one: "Agent", many: "agents" }}
      {...props}
    />,
  );
}

function setReducedMotion(matches: boolean) {
  window.matchMedia = ((query: string) => ({
    matches,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

describe("LiveList", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    setReducedMotion(false);
  });
  afterEach(() => {
    vi.runOnlyPendingTimers();
    vi.useRealTimers();
  });

  it("AC-1: renders an added row with the add tint (no manual action)", () => {
    const { rerender } = renderList({ items: [row("a")] });
    // First population never tints.
    expect(document.querySelector('[data-rp5-added="true"]')).toBeNull();

    act(() => {
      rerender(
        <LiveList<Row>
          items={[row("a"), row("b")]}
          getKey={(r) => r.id}
          renderRow={(r) => <span data-testid={`row-${r.id}`}>{r.name}</span>}
          getLabel={(r) => r.name}
          noun={{ one: "Agent", many: "agents" }}
        />,
      );
    });

    const tinted = document.querySelector('[data-rp5-added="true"]');
    expect(tinted).not.toBeNull();
    expect(within(tinted as HTMLElement).getByTestId("row-b")).toBeInTheDocument();

    // Tint clears after its window.
    act(() => vi.advanceTimersByTime(1300));
    expect(document.querySelector('[data-rp5-added="true"]')).toBeNull();
  });

  it("AC-2: user removal collapses; background eviction shows a Removed tombstone", () => {
    const props = {
      getKey: (r: Row) => r.id,
      renderRow: (r: Row) => <span data-testid={`row-${r.id}`}>{r.name}</span>,
      getLabel: (r: Row) => r.name,
      noun: { one: "Agent", many: "agents" },
    };
    const { rerender } = render(<LiveList<Row> items={[row("a"), row("b")]} {...props} />);

    // Background eviction of "a".
    act(() => {
      rerender(<LiveList<Row> items={[row("b")]} evictedKeys={["a"]} {...props} />);
    });
    // Tombstone still present (row "a" data rendered) with the Removed label.
    expect(screen.getByTestId("row-a")).toBeInTheDocument();
    expect(screen.getByText("Removed")).toBeInTheDocument();

    // After the tombstone window + collapse it is gone.
    act(() => vi.advanceTimersByTime(2000 + 150 + 20));
    expect(screen.queryByTestId("row-a")).toBeNull();

    // User removal of "b" collapses (no tombstone label) then drops.
    act(() => {
      rerender(<LiveList<Row> items={[]} {...props} />);
    });
    expect(screen.queryByText("Removed")).toBeNull();
    const collapsingWrapper = () => document.querySelector(".rp5-row");
    expect(collapsingWrapper()).not.toBeNull();
    act(() => vi.advanceTimersByTime(150 + 40));
    expect(screen.queryByTestId("row-b")).toBeNull();
  });

  it("marks exiting rows `inert` so a dead row takes no focus / a11y", () => {
    const props = {
      getKey: (r: Row) => r.id,
      renderRow: (r: Row) => <button data-testid={`btn-${r.id}`}>{r.name}</button>,
      getLabel: (r: Row) => r.name,
      noun: { one: "Agent", many: "agents" },
    };
    const { rerender } = render(<LiveList<Row> items={[row("a"), row("b")]} {...props} />);
    // Evict "a" → its wrapper is a tombstone; the still-rendered button must be inert
    // (keyboard-unreachable), since `pointer-events-none` blocks only the mouse.
    act(() => {
      rerender(<LiveList<Row> items={[row("b")]} evictedKeys={["a"]} {...props} />);
    });
    const exitWrapper = screen.getByTestId("btn-a").closest(".rp5-row");
    expect(exitWrapper).not.toBeNull();
    expect(exitWrapper).toHaveAttribute("inert");
    // The live row's wrapper is NOT inert.
    const liveWrapper = screen.getByTestId("btn-b").closest(".rp5-row");
    expect(liveWrapper).not.toHaveAttribute("inert");
  });

  it("AC-3: announces add/remove via aria-live=polite; nothing is assertive", () => {
    const { rerender } = renderList({ items: [row("a")] });
    const announcer = screen.getByTestId("live-list-announcer");
    expect(announcer).toHaveAttribute("aria-live", "polite");
    expect(document.querySelector('[aria-live="assertive"]')).toBeNull();

    act(() => {
      rerender(
        <LiveList<Row>
          items={[row("a"), row("b")]}
          getKey={(r) => r.id}
          renderRow={(r) => <span>{r.name}</span>}
          getLabel={(r) => r.name}
          noun={{ one: "Agent", many: "agents" }}
        />,
      );
    });
    expect(announcer.textContent).toBe("Agent added: Item b");
  });

  it("AC-3/5: a burst coalesces the announcement to a count", () => {
    const props = {
      getKey: (r: Row) => r.id,
      renderRow: (r: Row) => <span>{r.name}</span>,
      getLabel: (r: Row) => r.name,
      noun: { one: "Agent", many: "agents" },
    };
    const { rerender } = render(<LiveList<Row> items={[row("a")]} {...props} />);
    act(() => {
      rerender(<LiveList<Row> items={["a", "b", "c", "d", "e", "f", "g"].map(row)} {...props} />);
    });
    expect(screen.getByTestId("live-list-announcer").textContent).toBe("6 agents added");
  });

  it("AC-5: a burst suppresses per-row tints", () => {
    const props = {
      getKey: (r: Row) => r.id,
      renderRow: (r: Row) => <span>{r.name}</span>,
      getLabel: (r: Row) => r.name,
    };
    const { rerender } = render(<LiveList<Row> items={[row("a")]} {...props} />);
    act(() => {
      rerender(<LiveList<Row> items={["a", "b", "c", "d", "e", "f", "g"].map(row)} {...props} />);
    });
    expect(document.querySelector('[data-rp5-added="true"]')).toBeNull();
  });

  it("AC-4: reduced motion keeps the tombstone/added text cues + aria-live", () => {
    setReducedMotion(true);
    const props = {
      getKey: (r: Row) => r.id,
      renderRow: (r: Row) => <span data-testid={`row-${r.id}`}>{r.name}</span>,
      getLabel: (r: Row) => r.name,
      noun: { one: "Agent", many: "agents" },
    };
    const { rerender } = render(<LiveList<Row> items={[row("a"), row("b")]} {...props} />);

    // Added row: no tint under reduced motion, but the announcement remains.
    act(() => {
      rerender(<LiveList<Row> items={[row("a"), row("b"), row("c")]} {...props} />);
    });
    expect(document.querySelector('[data-rp5-added="true"]')).toBeNull();
    expect(screen.getByTestId("live-list-announcer").textContent).toBe("Agent added: Item c");

    // Evicted row: tombstone text remains, then drops with no collapse phase.
    act(() => {
      rerender(<LiveList<Row> items={[row("a"), row("c")]} evictedKeys={["b"]} {...props} />);
    });
    expect(screen.getByText("Removed")).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(2000 + 20));
    expect(screen.queryByText("Removed")).toBeNull();
  });

  it("AC-6: LiveCountBadge count derives from items; cross-fades; pulses on a burst", () => {
    const props = { getKey: (r: Row) => r.id };
    const { rerender } = render(<LiveCountBadge items={["a", "b"].map(row)} {...props} />);
    const badge = screen.getByTestId("live-count-badge");
    expect(badge.textContent).toBe("2");
    expect(badge).not.toHaveAttribute("data-rp5-pulse"); // first render never pulses

    // Small change: no pulse.
    act(() => rerender(<LiveCountBadge items={["a", "b", "c"].map(row)} {...props} />));
    expect(badge.textContent).toBe("3");
    expect(badge).not.toHaveAttribute("data-rp5-pulse");

    // Burst (>5 key churn): pulse, then auto-clear.
    act(() =>
      rerender(
        <LiveCountBadge
          items={["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"].map(row)}
          {...props}
        />,
      ),
    );
    expect(badge.textContent).toBe("10");
    expect(badge).toHaveAttribute("data-rp5-pulse", "true");
    act(() => vi.advanceTimersByTime(700));
    expect(badge).not.toHaveAttribute("data-rp5-pulse");
  });

  it("AC-5: badge pulses on a mixed add+remove burst even when net count is flat", () => {
    const props = { getKey: (r: Row) => r.id };
    // 4 items fully replaced by 4 others = 8 key churn (> 5) but net count 4 → 4.
    // The list would suppress tints; the badge must still pulse (same signal).
    const { rerender } = render(
      <LiveCountBadge items={["a", "b", "c", "d"].map(row)} {...props} />,
    );
    const badge = screen.getByTestId("live-count-badge");
    act(() => rerender(<LiveCountBadge items={["w", "x", "y", "z"].map(row)} {...props} />));
    expect(badge.textContent).toBe("4");
    expect(badge).toHaveAttribute("data-rp5-pulse", "true");
  });

  it("SW-AM-10: pulses when pulseToken changes even with a flat count (no burst)", () => {
    const props = { getKey: (r: Row) => r.id };
    const items = ["a", "b"].map(row);
    const { rerender } = render(<LiveCountBadge items={items} pulseToken={0} {...props} />);
    const badge = screen.getByTestId("live-count-badge");
    expect(badge).not.toHaveAttribute("data-rp5-pulse"); // first render never pulses

    // Same items (count unchanged, no burst); the token bump IS the "moved to
    // background" event — a nav-opened agent was already counted, so this is the
    // only signal the badge has.
    act(() => rerender(<LiveCountBadge items={items} pulseToken={1} {...props} />));
    expect(badge).toHaveAttribute("data-rp5-pulse", "true");
    act(() => vi.advanceTimersByTime(700));
    expect(badge).not.toHaveAttribute("data-rp5-pulse");
  });

  it("SW-AM-10 (AC-5): pulseToken does NOT pulse under reduced motion", () => {
    setReducedMotion(true);
    const props = { getKey: (r: Row) => r.id };
    const items = ["a", "b"].map(row);
    const { rerender } = render(<LiveCountBadge items={items} pulseToken={0} {...props} />);
    const badge = screen.getByTestId("live-count-badge");
    act(() => rerender(<LiveCountBadge items={items} pulseToken={1} {...props} />));
    expect(badge).not.toHaveAttribute("data-rp5-pulse");
  });

  it("AC-7: no Refresh button in the happy path; refetches on mount + focus", () => {
    const refetch = vi.fn();
    renderList({ items: [row("a")], refetch });
    // No refresh control anywhere.
    expect(screen.queryByRole("button", { name: /refresh/i })).toBeNull();
    // Refetched once on mount (pane open).
    expect(refetch).toHaveBeenCalledTimes(1);
    // Refetches on window focus.
    act(() => window.dispatchEvent(new Event("focus")));
    expect(refetch).toHaveBeenCalledTimes(2);
  });

  it("AC-7: an in-flight async refetch is not re-entered", () => {
    let resolve: (() => void) | undefined;
    const refetch = vi.fn(() => new Promise<void>((r) => (resolve = r)));
    renderList({ items: [row("a")], refetch });
    expect(refetch).toHaveBeenCalledTimes(1);
    act(() => window.dispatchEvent(new Event("focus")));
    // Still in flight → suppressed.
    expect(refetch).toHaveBeenCalledTimes(1);
    act(() => resolve?.());
  });

  it("AC-8: degraded banner shows only when broken; Retry refetches; rows persist", () => {
    const onRetry = vi.fn();
    const props = {
      getKey: (r: Row) => r.id,
      renderRow: (r: Row) => <span data-testid={`row-${r.id}`}>{r.name}</span>,
      recoverOnMount: false,
    };
    const { rerender } = render(<LiveList<Row> items={[row("a")]} {...props} />);
    expect(screen.queryByRole("status")).toBeNull();

    // Reconnecting: banner text, no button, rows stay.
    act(() =>
      rerender(
        <LiveList<Row> items={[row("a")]} degraded={{ status: "reconnecting" }} {...props} />,
      ),
    );
    expect(screen.getByRole("status")).toHaveTextContent(/reconnecting/i);
    expect(screen.getByTestId("row-a")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /retry/i })).toBeNull();

    // Failed: Retry button appears.
    act(() =>
      rerender(
        <LiveList<Row>
          items={[row("a")]}
          degraded={{ status: "failed" }}
          onRetry={onRetry}
          {...props}
        />,
      ),
    );
    act(() => screen.getByRole("button", { name: /retry/i }).click());
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("AC-8: recovery from degraded reconciles silently (no cues)", () => {
    const props = {
      getKey: (r: Row) => r.id,
      renderRow: (r: Row) => <span data-testid={`row-${r.id}`}>{r.name}</span>,
      getLabel: (r: Row) => r.name,
      noun: { one: "Agent", many: "agents" },
      recoverOnMount: false,
    };
    const { rerender } = render(
      <LiveList<Row> items={[row("a")]} degraded={{ status: "failed" }} {...props} />,
    );
    // Recover: degraded → null AND the list changes → silent (no tint/announce).
    act(() =>
      rerender(<LiveList<Row> items={[row("a"), row("b"), row("c")]} degraded={null} {...props} />),
    );
    expect(document.querySelector('[data-rp5-added="true"]')).toBeNull();
    expect(screen.getByTestId("live-list-announcer").textContent).toBe("");
  });

  it("AC-9: empty vs loading render the right slot", () => {
    const props = {
      getKey: (r: Row) => r.id,
      renderRow: (r: Row) => <span>{r.name}</span>,
      emptyState: <div data-testid="empty">Nothing here</div>,
      loadingState: <div data-testid="loading">Loading…</div>,
    };
    const { rerender } = render(<LiveList<Row> items={null} {...props} />);
    expect(screen.getByTestId("loading")).toBeInTheDocument();
    expect(screen.queryByTestId("empty")).toBeNull();

    act(() => rerender(<LiveList<Row> items={[]} {...props} />));
    expect(screen.getByTestId("empty")).toBeInTheDocument();
    expect(screen.queryByTestId("loading")).toBeNull();
  });

  it("re-adding an evicted key before its tombstone drops shows no duplicate", () => {
    const props = {
      getKey: (r: Row) => r.id,
      renderRow: (r: Row) => <span data-testid={`row-${r.id}`}>{r.name}</span>,
      getLabel: (r: Row) => r.name,
    };
    const { rerender } = render(<LiveList<Row> items={[row("a"), row("b")]} {...props} />);
    // Evict "a" → tombstone.
    act(() => rerender(<LiveList<Row> items={[row("b")]} evictedKeys={["a"]} {...props} />));
    expect(screen.getByText("Removed")).toBeInTheDocument();
    // Re-add "a" before the ~2s tombstone drops → tombstone cancelled, one live row.
    act(() => rerender(<LiveList<Row> items={[row("a"), row("b")]} {...props} />));
    expect(screen.queryByText("Removed")).toBeNull();
    expect(screen.getAllByTestId("row-a")).toHaveLength(1);
    // The stale exit timer must not remove the now-live row.
    act(() => vi.advanceTimersByTime(3000));
    expect(screen.getByTestId("row-a")).toBeInTheDocument();
  });

  it("a loading blip (items → null → populated) reconciles silently", () => {
    const props = {
      getKey: (r: Row) => r.id,
      renderRow: (r: Row) => <span data-testid={`row-${r.id}`}>{r.name}</span>,
      getLabel: (r: Row) => r.name,
      noun: { one: "Agent", many: "agents" },
    };
    const { rerender } = render(<LiveList<Row> items={[row("a"), row("b")]} {...props} />);
    act(() => rerender(<LiveList<Row> items={null} {...props} />));
    // Reload with a changed list → no phantom cues, no announcement.
    act(() => rerender(<LiveList<Row> items={[row("a"), row("c")]} {...props} />));
    expect(document.querySelector('[data-rp5-added="true"]')).toBeNull();
    expect(screen.queryByText("Removed")).toBeNull();
    expect(screen.getByTestId("live-list-announcer").textContent).toBe("");
  });

  it("AC-8: two-step recovery (degraded clears, then data arrives next tick) stays silent", () => {
    const props = {
      getKey: (r: Row) => r.id,
      renderRow: (r: Row) => <span data-testid={`row-${r.id}`}>{r.name}</span>,
      getLabel: (r: Row) => r.name,
      noun: { one: "Agent", many: "agents" },
      recoverOnMount: false,
    };
    const { rerender } = render(
      <LiveList<Row> items={[row("a")]} degraded={{ status: "failed" }} {...props} />,
    );
    // Store clears the degraded flag first, roster unchanged this render.
    act(() => rerender(<LiveList<Row> items={[row("a")]} degraded={null} {...props} />));
    // Corrected roster arrives a tick later — must reconcile silently.
    act(() =>
      rerender(<LiveList<Row> items={[row("a"), row("b"), row("c")]} degraded={null} {...props} />),
    );
    expect(document.querySelector('[data-rp5-added="true"]')).toBeNull();
    expect(screen.getByTestId("live-list-announcer").textContent).toBe("");
    // The latch is consumed — the NEXT real change animates normally.
    act(() =>
      rerender(
        <LiveList<Row>
          items={[row("a"), row("b"), row("c"), row("d")]}
          degraded={null}
          {...props}
        />,
      ),
    );
    expect(document.querySelector('[data-rp5-added="true"]')).not.toBeNull();
  });

  it("AC-5: a non-burst change during a pulse does not strand the badge pulse", () => {
    const props = { getKey: (r: Row) => r.id };
    const burst = ["a", "b", "c", "d", "e", "f", "g"].map(row);
    const { rerender } = render(<LiveCountBadge items={[row("a")]} {...props} />);
    const badge = screen.getByTestId("live-count-badge");
    // Burst → pulse.
    act(() => rerender(<LiveCountBadge items={burst} {...props} />));
    expect(badge).toHaveAttribute("data-rp5-pulse", "true");
    // A small (non-burst) change 300ms into the pulse re-runs the effect.
    act(() => vi.advanceTimersByTime(300));
    act(() => rerender(<LiveCountBadge items={[...burst, row("h")]} {...props} />));
    expect(badge).toHaveAttribute("data-rp5-pulse", "true");
    // The pulse still clears on its own 650ms window (timer not cancelled by the re-run).
    act(() => vi.advanceTimersByTime(400));
    expect(badge).not.toHaveAttribute("data-rp5-pulse");
  });

  it("AC-5: LiveCountBadge does not pulse on a loading blip (items → null → reload)", () => {
    const props = { getKey: (r: Row) => r.id };
    const roster = ["a", "b", "c", "d", "e", "f", "g", "h"].map(row);
    const { rerender } = render(<LiveCountBadge items={roster} {...props} />);
    const badge = screen.getByTestId("live-count-badge");
    act(() => rerender(<LiveCountBadge items={null} {...props} />));
    expect(badge).not.toHaveAttribute("data-rp5-pulse");
    expect(badge.textContent).toBe("0");
    // Reload the full roster → treated as first population, no burst pulse.
    act(() => rerender(<LiveCountBadge items={roster} {...props} />));
    expect(badge).not.toHaveAttribute("data-rp5-pulse");
    expect(badge.textContent).toBe("8");
  });

  it("a flapping added row re-arms its tint instead of being cut short", () => {
    const props = {
      getKey: (r: Row) => r.id,
      renderRow: (r: Row) => <span data-testid={`row-${r.id}`}>{r.name}</span>,
      getLabel: (r: Row) => r.name,
    };
    const tinted = () => document.querySelector('[data-rp5-added="true"]');
    const { rerender } = render(<LiveList<Row> items={[row("a")]} {...props} />);
    act(() => rerender(<LiveList<Row> items={[row("a"), row("b")]} {...props} />));
    expect(tinted()).not.toBeNull();
    // 800ms into b's tint, remove then re-add it (a flap) — its exit drops first.
    act(() => vi.advanceTimersByTime(800));
    act(() => rerender(<LiveList<Row> items={[row("a")]} {...props} />));
    act(() => vi.advanceTimersByTime(200));
    act(() => rerender(<LiveList<Row> items={[row("a"), row("b")]} {...props} />));
    // Past the ORIGINAL 1200ms deadline, the re-armed tint is still present.
    act(() => vi.advanceTimersByTime(400));
    expect(tinted()).not.toBeNull();
    // Once the re-armed window fully elapses it clears.
    act(() => vi.advanceTimersByTime(1100));
    expect(tinted()).toBeNull();
  });

  it("AC-8: failed state with no onRetry falls back to the refetch fn", () => {
    const refetch = vi.fn();
    render(
      <LiveList<Row>
        items={[row("a")]}
        getKey={(r) => r.id}
        renderRow={(r) => <span>{r.name}</span>}
        degraded={{ status: "failed" }}
        recoverOnMount={false}
        refetch={refetch}
      />,
    );
    act(() => screen.getByRole("button", { name: /retry/i }).click());
    expect(refetch).toHaveBeenCalledTimes(1);
  });

  it("a rejected async refetch does not wedge later refetches", async () => {
    const refetch = vi.fn().mockRejectedValueOnce(new Error("boom")).mockResolvedValue(undefined);
    renderList({ items: [row("a")], refetch });
    expect(refetch).toHaveBeenCalledTimes(1);
    // Let the rejected promise settle (releases the in-flight guard).
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    act(() => window.dispatchEvent(new Event("focus")));
    expect(refetch).toHaveBeenCalledTimes(2);
  });
});
