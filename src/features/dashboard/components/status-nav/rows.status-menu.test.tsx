// SPDX-License-Identifier: MIT
import { useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { renderRow, type RowInteractionModel } from "./rows";
import { buildStoryRef } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";

const path = "/repo/story.md";

afterEach(() => useStatusTaxonomyStore.getState().resetForTest());

it.each(["design", "needs-triage"])(
  "changes %s through the nav row and reopens with the new status checked",
  async (initial) => {
    useStatusTaxonomyStore.getState().setStates([
      {
        id: "design",
        label: "Needs design",
        role: "not_started",
        order: 0,
        color: "red",
        icon: "Circle",
      },
      {
        id: "building",
        label: "Under way",
        role: "active",
        order: 1,
        color: "blue",
        icon: "Circle",
      },
    ]);
    const onStatusChange = vi.fn();
    const onFileSelect = vi.fn();
    function Nav() {
      const [status, setStatus] = useState(initial);
      const [open, setOpen] = useState(false);
      const model: RowInteractionModel = {
        teamKey: "acme:team",
        selectedPath: null,
        onFileSelect,
        onContextMenu: vi.fn(),
        toggleExpand: vi.fn(),
        toggleGenericExpand: vi.fn(),
        statusPopoverOpenMap: new Map([[path, open]]),
        setStatusPopoverOpen: (_path, next) => setOpen(next),
        activeTeam: null,
        onTogglePinnedSection: vi.fn(),
        onPinnedSelect: vi.fn(),
        onPinnedUnpin: vi.fn(),
        onPinnedContextMenu: vi.fn(),
        onStatusChange: (file, next) => {
          onStatusChange(file, next);
          setStatus(next);
        },
      };
      return renderRow(
        {
          type: "story",
          story: buildStoryRef("story.md", { absolutePath: path, status }),
          depth: 0,
          isReady: false,
          blockedBy: [],
        },
        model,
      );
    }
    render(<Nav />);
    const user = userEvent.setup();
    const trigger = screen.getByRole("button", { name: "Change status" });
    await user.click(trigger);
    if (initial === "needs-triage") {
      expect(
        screen.getByRole("menu", { name: "Status, current: needs-triage" }),
      ).toBeInTheDocument();
      expect(trigger.querySelector('[data-rail-role="off"]')).toBeInTheDocument();
      expect(screen.queryByRole("menuitemradio", { checked: true })).toBeNull();
    } else {
      expect(screen.getByRole("menuitemradio", { name: "Needs design" })).toHaveAttribute(
        "aria-checked",
        "true",
      );
    }
    await user.click(screen.getByRole("menuitemradio", { name: "Under way" }));
    expect(onStatusChange).toHaveBeenCalledExactlyOnceWith(path, "building");
    expect(onFileSelect).not.toHaveBeenCalled();
    expect(trigger.querySelector('[data-rail-role="active"]')).toBeInTheDocument();
    await user.click(trigger);
    expect(screen.getByRole("menuitemradio", { name: "Under way" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(screen.getByRole("menuitemradio", { name: "Needs design" })).toHaveAttribute(
      "aria-checked",
      "false",
    );
  },
);
