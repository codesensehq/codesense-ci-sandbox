// SPDX-License-Identifier: MIT
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { MoveActionMenu } from "@/features/context-mobility/move-action-menu";
import { personalContextId, type Context } from "@/shared/lib/context-model";

const source: Context = {
  id: personalContextId(),
  kind: "personal",
  sharing: "personal",
  label: "Personal",
};

describe("MoveActionMenu", () => {
  it("the per-file ⋯ opens Move to… / Copy to… and starts the chosen verb", () => {
    const onStart = vi.fn();
    render(
      <MoveActionMenu source={source} logicalName="plan.md" fileName="plan.md" onStart={onStart} />,
    );
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    expect(screen.getByTestId("move-action-move")).toHaveTextContent("Move to…");
    expect(screen.getByTestId("move-action-copy")).toHaveTextContent("Copy to…");
    fireEvent.click(screen.getByTestId("move-action-move"));
    expect(onStart).toHaveBeenCalledWith("move");
  });

  it("Copy to… starts a copy", () => {
    const onStart = vi.fn();
    render(
      <MoveActionMenu source={source} logicalName="plan.md" fileName="plan.md" onStart={onStart} />,
    );
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    fireEvent.click(screen.getByTestId("move-action-copy"));
    expect(onStart).toHaveBeenCalledWith("copy");
  });

  it("labels the trigger for the file (accessible per-item action)", () => {
    render(
      <MoveActionMenu source={source} logicalName="plan.md" fileName="plan.md" onStart={vi.fn()} />,
    );
    expect(screen.getByTestId("move-action-trigger")).toHaveAttribute(
      "aria-label",
      "More actions for plan.md",
    );
  });

  it("SLW-PT-14: shows Remove… in the same ⋯ menu and starts remove", () => {
    const onRemove = vi.fn();
    render(
      <MoveActionMenu
        source={source}
        logicalName="plan.md"
        fileName="plan.md"
        onStart={vi.fn()}
        onRemove={onRemove}
      />,
    );
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    expect(screen.getByTestId("move-action-remove")).toHaveTextContent("Remove…");
    fireEvent.click(screen.getByTestId("move-action-remove"));
    expect(onRemove).toHaveBeenCalledTimes(1);
  });

  it("omits Remove… when no onRemove is wired (degrades cleanly)", () => {
    render(
      <MoveActionMenu source={source} logicalName="plan.md" fileName="plan.md" onStart={vi.fn()} />,
    );
    fireEvent.click(screen.getByTestId("move-action-trigger"));
    expect(screen.queryByTestId("move-action-remove")).toBeNull();
  });
});
