// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { TeamWorkspaceSkeleton } from "./team-workspace-skeleton";

describe("TeamWorkspaceSkeleton", () => {
  it("renders a structural skeleton with no spinner", () => {
    render(<TeamWorkspaceSkeleton />);
    const root = screen.getByTestId("team-workspace-skeleton");
    expect(root).toBeInTheDocument();
    // No spinner / Loader2 / role=progressbar — skeleton uses pulse, not
    // an indeterminate spinner.
    expect(root.querySelector('[role="progressbar"]')).toBeNull();
    expect(root.className).toContain("animate-pulse");
    // a11y: marked aria-busy so screen readers announce the loading state.
    expect(root.getAttribute("aria-busy")).toBe("true");
  });
});
