// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";

import { WorktreeSetupApprovalModal } from "./worktree-setup-approval-modal";

describe("WorktreeSetupApprovalModal", () => {
  it("points a skipped setup at the Retry the workspace really offers", () => {
    render(
      <WorktreeSetupApprovalModal
        request={{ steps: ["pnpm install"], repoLabel: "acme/api" }}
        onApprove={vi.fn()}
        onDecline={vi.fn()}
      />,
    );
    const description = screen.getByText(/came from your team/);
    expect(description.textContent).not.toMatch(/Test my setup/);
    expect(description.textContent).toMatch(/Retry/);
    expect(screen.getByText("1. pnpm install")).toBeTruthy();
  });
});
