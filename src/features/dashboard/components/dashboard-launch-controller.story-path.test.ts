// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { makeDashboardLaunchController } from "./dashboard-launch-controller";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

beforeEach(() => useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "a" }));
afterEach(() => vi.restoreAllMocks());

it.each([[undefined], [true]])(
  "a story-path open with headed=%s opens the native workspace with focus",
  async (headed) => {
    const open = vi
      .spyOn(useViewerStore.getState(), "openOrFocusStoryWorkspace")
      .mockResolvedValue("/wt/story");
    const controller = makeDashboardLaunchController("/ctx/a/panes/tree.html");
    await expect(
      controller.openWorkspace({
        target: { kind: "story", storyPath: "/ctx/a/stories/story.md" },
        headed,
      }),
    ).resolves.toEqual({});
    expect(open).toHaveBeenCalledOnce();
    expect(open).toHaveBeenCalledWith(
      "acme:a",
      "/ctx/a/stories/story.md",
      undefined,
      undefined,
      true,
    );
  },
);
