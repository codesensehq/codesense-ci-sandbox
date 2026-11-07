// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Scan } from "lucide-react";

import { render } from "@/test-utils";
import { ChromeIconButton } from "./chrome-icon-button";

describe("ChromeIconButton (SW-PP-3)", () => {
  it("renders an icon button named by its title and fires onClick", () => {
    const onClick = vi.fn();
    render(<ChromeIconButton icon={Scan} title="Fit pipeline to view" onClick={onClick} />);

    const btn = screen.getByRole("button", { name: "Fit pipeline to view" });
    fireEvent.click(btn);
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("reveals its title as an instant Radix tooltip on hover (SW-FX-80)", async () => {
    const user = userEvent.setup();
    render(<ChromeIconButton icon={Scan} title="Fit pipeline to view" onClick={() => {}} />);

    await user.hover(screen.getByRole("button", { name: "Fit pipeline to view" }));
    expect(await screen.findByRole("tooltip")).toHaveTextContent("Fit pipeline to view");
  });

  it("stops propagation so a click never reaches the section header (collapse/drag)", () => {
    const onClick = vi.fn();
    const onParentClick = vi.fn();
    render(
      <div onClick={onParentClick}>
        <ChromeIconButton icon={Scan} title="Fit pipeline to view" onClick={onClick} />
      </div>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Fit pipeline to view" }));
    expect(onClick).toHaveBeenCalledTimes(1);
    expect(onParentClick).not.toHaveBeenCalled();
  });

  it("activates via keyboard (Enter) when enabled — the refactored overlays rely on native button semantics", async () => {
    const user = userEvent.setup();
    const onClick = vi.fn();
    render(<ChromeIconButton icon={Scan} title="Fit pipeline to view" onClick={onClick} />);

    const btn = screen.getByRole("button", { name: "Fit pipeline to view" });
    btn.focus();
    await user.keyboard("{Enter}");
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("when disabled: aria-disabled, out of tab order, and inert on click", () => {
    const onClick = vi.fn();
    render(
      <ChromeIconButton icon={Scan} title="Fit pipeline to view" onClick={onClick} disabled />,
    );

    const btn = screen.getByRole("button", { name: "Fit pipeline to view" });
    expect(btn).toHaveAttribute("aria-disabled", "true");
    expect(btn).toHaveAttribute("tabindex", "-1");
    fireEvent.click(btn);
    expect(onClick).not.toHaveBeenCalled();
  });
});
