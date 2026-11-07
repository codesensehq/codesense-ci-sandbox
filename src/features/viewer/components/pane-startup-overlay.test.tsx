// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useEffect } from "react";
import { act, fireEvent, render, screen } from "@testing-library/react";

import {
  PANE_SPINNER_DELAY_MS,
  PANE_START_TIMEOUT_MS,
  PANE_STILL_STARTING_MS,
  usePaneStartup,
  type PaneStartup,
} from "../hooks/use-pane-startup";
import { PaneStartupOverlay, RetryRemount } from "./pane-startup-overlay";

// The pane's startup feedback as the terminal panel wires it: the hook drives the
// overlay, the panel reports ready / rendered / failed, and Retry rebuilds the pane.
const live: { pane: PaneStartup | null; mounts: number } = { pane: null, mounts: 0 };

interface PaneProps {
  visible: boolean;
  started?: boolean;
  restoring?: boolean;
  onRetry: () => void;
}

function Pane({ visible, started = false, restoring = false, onRetry }: PaneProps) {
  const startup = usePaneStartup(visible);
  useEffect(() => {
    live.pane = startup;
  });
  return (
    <PaneStartupOverlay
      phase={startup.phase}
      failure={startup.failure}
      started={started}
      restoring={restoring}
      background="#000"
      onRetry={onRetry}
    />
  );
}

function Mounted(props: PaneProps) {
  useEffect(() => {
    live.mounts++;
  }, []);
  return <Pane {...props} />;
}

function renderPane(visible = true, started = false, restoring = false) {
  const view = render(
    <RetryRemount>
      {(retry) => (
        <Mounted visible={visible} started={started} restoring={restoring} onRetry={retry} />
      )}
    </RetryRemount>,
  );
  return {
    ...view,
    setVisible: (v: boolean) =>
      view.rerender(
        <RetryRemount>{(retry) => <Mounted visible={v} onRetry={retry} />}</RetryRemount>,
      ),
  };
}

const overlay = () => screen.queryByTestId("pane-startup-overlay");
const advance = (ms: number) => act(() => void vi.advanceTimersByTime(ms));
const pane = () => live.pane!;
const paint = () =>
  act(() => {
    pane().markReady();
    pane().markRendered();
  });

beforeEach(() => {
  vi.useFakeTimers();
  live.mounts = 0;
});
afterEach(() => vi.useRealTimers());

describe("a pane is never silently blank", () => {
  it("a fast open paints before anything shows", () => {
    renderPane();
    advance(PANE_SPINNER_DELAY_MS - 1);
    paint();
    advance(PANE_START_TIMEOUT_MS);
    expect(overlay()).toBeNull();
  });

  it("a slow open shows the spinner, then 'Still starting…' at 2 s, then clears on paint", () => {
    renderPane();
    advance(PANE_SPINNER_DELAY_MS);
    expect(overlay()).toHaveAttribute("data-phase", "starting");
    expect(screen.getByRole("status")).toHaveTextContent("Initializing terminal…");
    advance(PANE_STILL_STARTING_MS - PANE_SPINNER_DELAY_MS);
    expect(screen.getByRole("status")).toHaveTextContent("Still starting…");
    paint();
    expect(overlay()).toBeNull();
  });

  it("content that the renderer never draws is still a blank pane", () => {
    renderPane();
    act(() => pane().markReady());
    advance(PANE_STILL_STARTING_MS);
    expect(screen.getByRole("status")).toHaveTextContent("Still starting…");
  });

  it("a frame drawn before the content arrives doesn't count as the first paint", () => {
    renderPane();
    act(() => void pane().markRendered());
    act(() => pane().markReady());
    advance(PANE_STILL_STARTING_MS);
    expect(overlay()).not.toBeNull();
    act(() => void pane().markRendered());
    expect(overlay()).toBeNull();
  });

  it("no paint by the timeout, before any attach → 'Failed to start' with Retry", () => {
    renderPane();
    advance(PANE_START_TIMEOUT_MS);
    expect(overlay()).toHaveAttribute("data-phase", "failed");
    expect(screen.getByRole("alert")).toHaveTextContent("Failed to start");
    expect(screen.getByRole("alert")).toHaveTextContent("The terminal didn't start in time.");
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
  });

  it("attached but never drawn by the timeout → says it couldn't be shown, not that it failed", () => {
    renderPane(true, true);
    act(() => pane().markReady());
    advance(PANE_START_TIMEOUT_MS);
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("Couldn't show the terminal");
    expect(alert).toHaveTextContent("The terminal couldn't be shown.");
    expect(alert).not.toHaveTextContent("Failed to start");
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
  });

  it("a restore shows only the background for 300 ms, then says it is restoring", () => {
    renderPane(true, false, true);
    advance(PANE_SPINNER_DELAY_MS - 1);
    expect(overlay()).toBeNull();
    advance(1);
    expect(screen.getByRole("status")).toHaveTextContent("Restoring session…");
    advance(PANE_STILL_STARTING_MS);
    expect(screen.getByRole("status")).toHaveTextContent("Restoring session…");
  });

  it("a restore that failed says so above Retry", () => {
    renderPane(true, false, true);
    act(() =>
      pane().markFailed("The session is still running.", "Couldn't restore this terminal."),
    );
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("Couldn't restore this terminal.");
    expect(alert).not.toHaveTextContent("Failed to start");
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
  });

  it("Retry rebuilds the pane and starts its clocks over", () => {
    renderPane();
    advance(PANE_START_TIMEOUT_MS);
    expect(live.mounts).toBe(1);
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(live.mounts).toBe(2);
    expect(overlay()).toBeNull();
    advance(PANE_STILL_STARTING_MS);
    expect(screen.getByRole("status")).toHaveTextContent("Still starting…");
  });

  it("a late paint after the timeout still clears the failure", () => {
    renderPane();
    advance(PANE_START_TIMEOUT_MS);
    paint();
    expect(overlay()).toBeNull();
  });

  it("the clocks run only while the pane is on screen, and restart when it is shown", () => {
    const view = renderPane(false);
    advance(PANE_START_TIMEOUT_MS * 2);
    expect(overlay()).toBeNull();
    view.setVisible(true);
    advance(PANE_STILL_STARTING_MS);
    expect(screen.getByRole("status")).toHaveTextContent("Still starting…");
    view.setVisible(false);
    expect(overlay()).toBeNull();
    view.setVisible(true);
    expect(overlay()).toBeNull();
    advance(PANE_SPINNER_DELAY_MS);
    expect(overlay()).toHaveAttribute("data-phase", "starting");
  });
});
