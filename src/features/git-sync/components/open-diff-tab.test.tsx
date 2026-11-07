// SPDX-License-Identifier: MIT
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({
  read: vi.fn(),
  review: vi.fn(),
  workspace: vi.fn(),
  team: vi.fn(),
  management: vi.fn(),
}));
vi.mock("@/shared/lib/open-file-in-team", () => ({ openFileInTeam: mocks.team }));
vi.mock("@/shared/lib/open-management-tab", () => ({ openManagementTab: mocks.management }));
vi.mock("../lib/source-redline-source", () => ({ readOpenDiff: mocks.read }));
vi.mock("@/shared/components/code-editor/CodeDiffReview", () => ({
  CodeDiffReview: (props: unknown) => {
    mocks.review(props);
    return <div>Read-only diff</div>;
  },
}));
vi.mock("@/features/dashboard/components/html-bridge", () => ({
  HTML_BRIDGE_ALLOWLIST: { openWorkspace: mocks.workspace },
}));
import { OpenDiffTab } from "./open-diff-tab";
const params = {
  repo: "acme/app",
  rev: "abcdef1",
  path: "a.ts",
  storyId: "SV-X",
  range: { start: 2, end: 3 },
};
const path = "virtual://source-diff/" + encodeURIComponent(JSON.stringify(params));
const opened = {
  status: "opened",
  redline: { before: "old", after: "new", binary: false },
  files: [{ path: "a.ts" }],
  resolvedRev: "abcdef1",
};
beforeEach(() => vi.clearAllMocks());
it("revalidates on mount and Refresh, discards superseded replies and passes the after range", async () => {
  const replies: ((v: unknown) => void)[] = [];
  mocks.read.mockImplementation(() => new Promise((resolve) => replies.push(resolve)));
  render(<OpenDiffTab filePath={path} />);
  await act(async () => {});
  expect(mocks.read).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByText("Refresh"));
  await act(async () => {});
  expect(mocks.read).toHaveBeenCalledTimes(2);
  await act(async () => replies[0]({ ...opened, redline: { before: "stale", after: "stale" } }));
  expect(mocks.review).not.toHaveBeenCalled();
  expect(mocks.read).toHaveBeenCalledTimes(2);
  await act(async () => replies[1](opened));
  expect(mocks.review.mock.calls.at(-1)?.[0]).toMatchObject({
    before: "old",
    after: "new",
    afterRange: params.range,
    collapseUnchanged: false,
  });
  fireEvent.click(screen.getByText("Open in workspace"));
  await waitFor(() => expect(mocks.workspace).toHaveBeenCalledWith({ storyId: "SV-X" }));
});
it("shows a selectable fetch command and keeps a removed worktree honest on a fresh tab read", async () => {
  mocks.read
    .mockResolvedValueOnce({
      status: "commitNotLocal",
      reason: "This commit isn't on this Mac yet",
      fetchCommand: "git fetch origin abcdef1",
    })
    .mockResolvedValueOnce({ status: "worktreeRemoved", reason: "The worktree was removed" });
  const first = render(<OpenDiffTab filePath={path} />);
  expect(await screen.findByDisplayValue("git fetch origin abcdef1")).toHaveAttribute("readonly");
  first.unmount();
  render(<OpenDiffTab filePath={path} />);
  expect(await screen.findByText("The worktree was removed")).toBeVisible();
  expect(mocks.read).toHaveBeenCalledTimes(2);
  expect(mocks.review).not.toHaveBeenCalled();
});
it("a malformed tab address is refused without a read", async () => {
  render(<OpenDiffTab filePath={"virtual://source-diff/" + encodeURIComponent("{not json")} />);
  expect(screen.getByRole("status").textContent).toContain("This diff address is not valid");
  render(
    <OpenDiffTab
      filePath={"virtual://source-diff/" + encodeURIComponent(JSON.stringify({ repo: "acme/app" }))}
    />,
  );
  expect(screen.getAllByRole("status").at(-1)?.textContent).toContain(
    "This diff address is not valid",
  );
  expect(mocks.read).not.toHaveBeenCalled();
});
it("switching file keeps the tab's team context", async () => {
  const team = { org: "acme", team: "dev" };
  mocks.read.mockResolvedValue({ ...opened, files: [{ path: "a.ts" }, { path: "b.ts" }] });
  render(
    <OpenDiffTab
      filePath={"virtual://source-diff/" + encodeURIComponent(JSON.stringify({ ...params, team }))}
    />,
  );
  await waitFor(() => expect(screen.getByLabelText("File")).toBeTruthy());
  expect(mocks.read.mock.calls[0][0]).not.toHaveProperty("team");
  fireEvent.change(screen.getByLabelText("File"), { target: { value: "b.ts" } });
  expect(mocks.management).not.toHaveBeenCalled();
  const [target, options] = mocks.team.mock.calls[0];
  expect(target).toMatchObject(team);
  expect(
    JSON.parse(decodeURIComponent(target.path.slice("virtual://source-diff/".length))),
  ).toEqual({ repo: "acme/app", rev: "abcdef1", path: "b.ts", storyId: "SV-X", team });
  expect(options).toMatchObject({ newTab: true });
});
