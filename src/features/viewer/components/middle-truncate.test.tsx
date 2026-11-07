// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { render, screen } from "@/test-utils";

import { MiddleTruncate } from "./middle-truncate";

describe("MiddleTruncate", () => {
  it("keeps the space at the head/tail seam (no 'Notificationstriage')", () => {
    render(<MiddleTruncate data-testid="mt" text="Notifications triage" />);
    const root = screen.getByTestId("mt");
    expect(root.textContent).toBe("Notifications triage");
    const head = root.firstElementChild as HTMLElement;
    expect(head.textContent).toBe("Notifications ");
    // `nowrap` would collapse that trailing space when rendered; `pre` keeps it.
    expect(head.className).toContain("whitespace-pre");
    expect(head.className).not.toMatch(/\btruncate\b/);
  });
});
