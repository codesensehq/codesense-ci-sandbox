// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { useViewerStore } from "./viewer-store";

// SW-VE-8: `requestEditActiveFile` is the store seam the `viewer.edit-file`
// shortcut/menu drives (the registry run() can't reach the view's edit gate).
describe("SW-VE-8 requestEditActiveFile", () => {
  beforeEach(() => {
    useViewerStore.setState({ editFileRequest: null });
  });

  it("starts null and records a request for a team", () => {
    expect(useViewerStore.getState().editFileRequest).toBeNull();
    useViewerStore.getState().requestEditActiveFile("org:team-a");
    const req = useViewerStore.getState().editFileRequest;
    expect(req).toEqual({ nonce: 1, teamKey: "org:team-a" });
  });

  it("bumps the nonce each call so re-firing the SAME file re-triggers", () => {
    useViewerStore.getState().requestEditActiveFile("org:team-a");
    useViewerStore.getState().requestEditActiveFile("org:team-a");
    expect(useViewerStore.getState().editFileRequest?.nonce).toBe(2);
  });

  it("carries the requesting teamKey (view filters on it)", () => {
    useViewerStore.getState().requestEditActiveFile("org:team-a");
    useViewerStore.getState().requestEditActiveFile("org:team-b");
    expect(useViewerStore.getState().editFileRequest).toEqual({ nonce: 2, teamKey: "org:team-b" });
  });
});
