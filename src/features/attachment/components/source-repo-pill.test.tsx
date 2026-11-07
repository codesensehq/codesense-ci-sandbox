// SPDX-License-Identifier: MIT
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, it, vi } from "vitest";
const { exists, detach } = vi.hoisted(() => ({ exists: vi.fn(), detach: vi.fn() }));
vi.mock("@/tauri/commands", () => ({
  checkPathExists: exists,
  deleteSourceRepoAttachment: detach,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));
vi.mock("@/shared/lib/get-current-user", () => ({
  getCurrentUser: () => ({ github_user_id: 42 }),
}));
vi.mock("@/shared/lib/commit-registry-edit", () => ({ commitRegistryEdit: vi.fn() }));
vi.mock("./attach-local-clone-dialog", () => ({
  AttachLocalCloneDialog: () => <div>Choose existing folder</div>,
}));
import { SourceRepoPill } from "./source-repo-pill";
const attachment = {
  repo_path: "/existing/repo",
  remote_url: "https://github.com/acme/app",
  attached_at: null,
  tracking_prompt_dismissed_at: null,
  managed: true,
  credential_ref: null,
};
beforeEach(() => {
  vi.clearAllMocks();
  exists.mockResolvedValue(true);
  detach.mockResolvedValue({});
});
it("treats legacy managed repositories as local attachments without transport controls", async () => {
  render(
    <SourceRepoPill
      slug={{ owner: "acme", repo: "app" }}
      attachment={attachment}
      orgSlug="acme"
      teamName="default"
    />,
  );
  await userEvent.click(screen.getByTestId("settings-source-repo-acme/app"));
  expect(screen.getByText("/existing/repo")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: /^Detach$/ })).toBeInTheDocument();
  expect(screen.queryByText(/Re-clone|Update \(pull\)|Remove clone|Acts as/)).toBeNull();
});
it("offers attaching an existing folder when a legacy checkout is unavailable", async () => {
  exists.mockResolvedValue(false);
  render(
    <SourceRepoPill
      slug={{ owner: "acme", repo: "app" }}
      attachment={attachment}
      orgSlug="acme"
      teamName="default"
    />,
  );
  await waitFor(() =>
    expect(screen.getByTestId("settings-source-repo-acme/app")).toHaveAttribute(
      "aria-label",
      "acme/app: folder unavailable",
    ),
  );
  await userEvent.click(screen.getByTestId("settings-source-repo-acme/app"));
  await userEvent.click(screen.getByRole("button", { name: "Re-attach folder" }));
  expect(screen.getByText("Choose existing folder")).toBeInTheDocument();
  expect(detach).not.toHaveBeenCalled();
});
