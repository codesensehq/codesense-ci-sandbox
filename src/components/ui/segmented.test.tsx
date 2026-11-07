// SPDX-License-Identifier: MIT
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { Segmented } from "./segmented";

const OPTIONS = [
  { value: "personal", label: "Only me" },
  { value: "team", label: "Team" },
  { value: "locked", label: "Locked", disabled: true },
];

describe("Segmented", () => {
  it("is a radiogroup with the selected option checked", () => {
    render(
      <Segmented aria-label="Where" value="team" onValueChange={() => {}} options={OPTIONS} />,
    );
    expect(screen.getByRole("radiogroup", { name: "Where" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Team" })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("radio", { name: "Only me" })).toHaveAttribute("aria-checked", "false");
  });

  it("reports a click as the new value", async () => {
    const onValueChange = vi.fn();
    render(
      <Segmented aria-label="Where" value="team" onValueChange={onValueChange} options={OPTIONS} />,
    );
    await userEvent.click(screen.getByRole("radio", { name: "Only me" }));
    expect(onValueChange).toHaveBeenCalledWith("personal");
  });

  it("does not select a disabled option", async () => {
    const onValueChange = vi.fn();
    render(
      <Segmented aria-label="Where" value="team" onValueChange={onValueChange} options={OPTIONS} />,
    );
    await userEvent.click(screen.getByRole("radio", { name: "Locked" }));
    expect(onValueChange).not.toHaveBeenCalled();
  });

  it("moves the selection with the arrow keys, skipping disabled options", async () => {
    function Harness() {
      const [v, setV] = useState("personal");
      return <Segmented aria-label="Where" value={v} onValueChange={setV} options={OPTIONS} />;
    }
    render(<Harness />);
    screen.getByRole("radio", { name: "Only me" }).focus();
    await userEvent.keyboard("{ArrowRight}");
    expect(screen.getByRole("radio", { name: "Team" })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("radio", { name: "Team" })).toHaveFocus();
    await userEvent.keyboard("{ArrowRight}");
    expect(screen.getByRole("radio", { name: "Only me" })).toHaveAttribute("aria-checked", "true");
  });
});
