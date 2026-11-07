// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

import { ReconcileBar, ReconcileErrorBanner, StaleConflictBanner } from "./conflict-banners";

// The shared SW-VE-4 conflict UI. Behavior in-context is covered by both inset
// suites (source-editor-inset / context-editor-inset); this locks the shared
// contract directly — the `testIdPrefix` wiring and handler dispatch that keep
// the worktree and context surfaces from diverging.

describe("conflict-banners — shared SW-VE-4 conflict UI", () => {
  it("StaleConflictBanner keys every hook off the prefix and dispatches each action", () => {
    const onResolve = vi.fn();
    const onReload = vi.fn();
    const onKeepEditing = vi.fn();
    render(
      <StaleConflictBanner
        testIdPrefix="ctx"
        onResolve={onResolve}
        onReload={onReload}
        onKeepEditing={onKeepEditing}
      />,
    );
    expect(screen.getByTestId("ctx-stale-warning")).toBeInTheDocument();

    fireEvent.click(screen.getByTestId("ctx-stale-resolve"));
    fireEvent.click(screen.getByTestId("ctx-stale-reload"));
    fireEvent.click(screen.getByTestId("ctx-stale-keep"));
    expect(onResolve).toHaveBeenCalledTimes(1);
    expect(onReload).toHaveBeenCalledTimes(1);
    expect(onKeepEditing).toHaveBeenCalledTimes(1);
  });

  it("ReconcileBar keys every hook off the prefix and dispatches each action", () => {
    const onTakeTheirs = vi.fn();
    const onKeepMine = vi.fn();
    const onCancel = vi.fn();
    render(
      <ReconcileBar
        testIdPrefix="wt"
        onTakeTheirs={onTakeTheirs}
        onKeepMine={onKeepMine}
        onCancel={onCancel}
      />,
    );
    expect(screen.getByTestId("wt-reconcile-bar")).toBeInTheDocument();

    fireEvent.click(screen.getByTestId("wt-reconcile-take-theirs"));
    fireEvent.click(screen.getByTestId("wt-reconcile-keep-mine"));
    fireEvent.click(screen.getByTestId("wt-reconcile-cancel"));
    expect(onTakeTheirs).toHaveBeenCalledTimes(1);
    expect(onKeepMine).toHaveBeenCalledTimes(1);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("ReconcileErrorBanner shows the reassuring copy under the prefixed testid", () => {
    render(<ReconcileErrorBanner testIdPrefix="ctx" message="boom" />);
    const el = screen.getByTestId("ctx-reconcile-error");
    expect(el).toBeInTheDocument();
    // SW-FX-80: the raw error no longer rides a native `title`; the banner shows
    // only the reassuring, non-alarming line.
    expect(screen.getByText(/your edits are safe/i)).toBeInTheDocument();
  });
});
