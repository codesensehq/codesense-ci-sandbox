// SPDX-License-Identifier: MIT
import { renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { getDashboardLaunchController, setDashboardLaunchController } from "./dashboard-launch";
import { useRegisterDashboardLaunchController } from "./dashboard-launch-controller";
import { runBridgeCommand } from "./html-bridge";

// Several live docs are mounted at once (a centre dashboard beside right-pane
// panes): each doc's bridge calls act for that doc, and one doc going away never
// takes the launch surface from the others.
describe("dashboard launch controller, one per live doc", () => {
  afterEach(() => setDashboardLaunchController(null));

  it("a doc's bridge call resolves against its own doc, whichever doc mounted last", async () => {
    const pane = renderHook(() =>
      useRegisterDashboardLaunchController("/org/backend/panes/board.html", true),
    );
    const centre = renderHook(() =>
      useRegisterDashboardLaunchController("/org/backend/content/dash/index.html", true),
    );
    expect(getDashboardLaunchController()).toBe(centre.result.current);

    await expect(
      runBridgeCommand("getDocDir", {}, { controller: pane.result.current }),
    ).resolves.toBe("/org/backend/panes");
    await expect(
      runBridgeCommand("getDocDir", {}, { controller: centre.result.current }),
    ).resolves.toBe("/org/backend/content/dash");

    // Closing the centre dashboard leaves the pane's launch surface intact.
    centre.unmount();
    expect(getDashboardLaunchController()).toBe(pane.result.current);
    await expect(
      runBridgeCommand("getDocDir", {}, { controller: pane.result.current }),
    ).resolves.toBe("/org/backend/panes");

    pane.unmount();
    expect(getDashboardLaunchController()).toBeNull();
  });

  it("unmounting an older doc keeps the newer doc's app-wide registration", () => {
    const older = renderHook(() => useRegisterDashboardLaunchController("/a.html", true));
    const newer = renderHook(() => useRegisterDashboardLaunchController("/b.html", true));
    older.unmount();
    expect(getDashboardLaunchController()).toBe(newer.result.current);
    newer.unmount();
    expect(getDashboardLaunchController()).toBeNull();
  });

  it("a baseline render has no controller and its calls fail closed", async () => {
    const baseline = renderHook(() => useRegisterDashboardLaunchController("/a.html", false));
    expect(baseline.result.current).toBeNull();
    await expect(
      runBridgeCommand(
        "openWorkspace",
        { target: { kind: "story", storyId: "X-1" } },
        {
          controller: null,
        },
      ),
    ).rejects.toThrow(/unavailable/);
  });
});
