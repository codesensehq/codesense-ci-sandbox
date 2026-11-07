// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { render, screen } from "@/test-utils";
import { SubagentMark } from "@/features/viewer/components/subagent-mark";

describe("SubagentMark", () => {
  it("draws the count with its words as its name and its tooltip", () => {
    render(<SubagentMark count={3} />);
    const mark = screen.getByRole("img", { name: "3 subagents running" });
    expect(mark).toHaveAttribute("title", "3 subagents running");
    expect(mark).toHaveAttribute("data-count", "3");
    expect(mark).toHaveTextContent("3");
    expect(mark).toHaveAttribute("data-testid", "agent-subagents");
  });

  it("names one subagent in the singular, and takes another test id", () => {
    render(<SubagentMark count={1} testId="popup-subagents" />);
    expect(screen.getByTestId("popup-subagents")).toHaveAccessibleName("1 subagent running");
  });

  it("draws nothing with no subagents running", () => {
    const { container } = render(<SubagentMark count={0} />);
    expect(container).toBeEmptyDOMElement();
  });
});
