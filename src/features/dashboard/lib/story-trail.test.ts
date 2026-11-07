// SPDX-License-Identifier: MIT
import { expect, it, vi } from "vitest";
const list = vi.hoisted(() => vi.fn());
vi.mock("@/tauri/commands", () => ({ listBuiltinPanes: list }));
import { openStoryTrail } from "./story-trail";
it("opens the materialized builtin in a new file tab with its story fragment", async () => {
  list.mockResolvedValue({
    panes: [{ id: "story-trail", path: "/h/users/42/builtin-panes/panes/story-trail.html" }],
  });
  const navigate = vi.fn();
  await openStoryTrail({ storyId: "SV-X", teamKey: "acme/api:dev", navigate });
  expect(list).toHaveBeenCalledWith("acme/api", "dev");
  expect(navigate).toHaveBeenCalledWith("/h/users/42/builtin-panes/panes/story-trail.html", {
    openInNewTab: true,
    hash: "#story/SV-X",
  });
});
