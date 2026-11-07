// SPDX-License-Identifier: MIT
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { useMaximized } from "./use-maximized";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
afterEach(cleanup);
it("retains logical content ownership and foreground gating across team switches", () => {
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "planning" });
  useViewerStore.setState({ maximizedPanel: { "acme:planning": "custom-pane" } });
  const { result } = renderHook(() => useMaximized("custom-pane", "team:acme:planning"));
  expect(result.current).toEqual({ ownTeamKey: "acme:planning", isFullscreen: true });
  act(() => useOrgStore.setState({ activeTeamName: "other" }));
  expect(result.current).toEqual({ ownTeamKey: "acme:planning", isFullscreen: false });
});
