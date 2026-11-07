// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { render, screen, within } from "@testing-library/react";

import { AppButton } from "./app-button";
import { TabHeader, TabSection, TabSurface } from "./tab-surface";

describe("TabSurface", () => {
  it("renders a scrollable themed surface with its test id", () => {
    render(
      <TabSurface testId="surface">
        <p>body</p>
      </TabSurface>,
    );
    const surface = screen.getByTestId("surface");
    expect(surface).toHaveAttribute("data-slot", "tab-surface");
    expect(surface.className).toContain("overflow-y-auto");
    expect(surface.className).toContain("text-foreground");
    expect(surface).toHaveTextContent("body");
  });
});

describe("TabHeader", () => {
  it("puts the title on the left and the actions top-right", () => {
    render(
      <TabHeader
        title="Source repos"
        description="The code this team works over."
        actions={<AppButton>Add repo</AppButton>}
      />,
    );
    const header = screen.getByRole("banner");
    expect(within(header).getByRole("heading", { name: "Source repos" })).toBeInTheDocument();
    expect(header).toHaveTextContent("The code this team works over.");
    const actions = header.querySelector('[data-slot="tab-header-actions"]');
    expect(actions).not.toBeNull();
    expect(within(actions as HTMLElement).getByRole("button", { name: "Add repo" })).toBeTruthy();
  });

  it("omits the actions slot when there are none", () => {
    render(<TabHeader title="t" />);
    expect(document.querySelector('[data-slot="tab-header-actions"]')).toBeNull();
  });
});

describe("TabSection", () => {
  it("renders a titled section with its content", () => {
    render(
      <TabSection title="Tracked repos" testId="sec">
        <p>rows</p>
      </TabSection>,
    );
    const section = screen.getByTestId("sec");
    expect(within(section).getByRole("heading", { name: "Tracked repos" })).toBeInTheDocument();
    expect(section).toHaveTextContent("rows");
  });
});
