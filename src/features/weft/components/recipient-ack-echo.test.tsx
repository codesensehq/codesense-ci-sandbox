// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { render, screen } from "@/test-utils";

import { RecipientAckEcho } from "./recipient-ack-echo";

describe("SW-WV-F5 · RecipientAckEcho (AC-5)", () => {
  it("renders seen + ack chips and omits a zero declined chip", () => {
    render(<RecipientAckEcho acks={{ seen: 2, acknowledged: 1, declined: 0 }} />);
    expect(screen.getByTestId("ack-chip-seen")).toHaveTextContent("seen 2");
    expect(screen.getByTestId("ack-chip-acknowledged")).toHaveTextContent("ack 1");
    expect(screen.queryByTestId("ack-chip-declined")).not.toBeInTheDocument();
  });

  it("tones the declined chip destructive when declined > 0", () => {
    render(<RecipientAckEcho acks={{ seen: 0, acknowledged: 0, declined: 1 }} />);
    const chip = screen.getByTestId("ack-chip-declined");
    expect(chip).toHaveTextContent("declined 1");
    expect(chip.className).toContain("text-destructive");
  });

  it("renders nothing when all counts are zero", () => {
    const { container } = render(
      <RecipientAckEcho acks={{ seen: 0, acknowledged: 0, declined: 0 }} />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});
