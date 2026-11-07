// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { useViewerStore } from "./viewer-store";

/**
 * SW-AU-85A-iii: the runtime-only open-file lifecycle slice. The viewer publishes its
 * own `readFile` outcome (the bit that drives `FileDeletedPlaceholder`) here, keyed by
 * normalized absolute path; the editor tab reads it to forward an honest `lifecycle`.
 */
describe("openFileLifecycleByPath", () => {
  beforeEach(() => {
    useViewerStore.setState({ openFileLifecycleByPath: {} });
  });

  const path = "/ctx/orgs/o/stories/SW-AU-85A-iii.md";

  it("sets a non-active lifecycle for a path", () => {
    useViewerStore.getState().setOpenFileLifecycle(path, "deleted");
    expect(useViewerStore.getState().openFileLifecycleByPath[path]).toBe("deleted");
  });

  it("deletes the entry when cleared with null (absence ≡ active)", () => {
    useViewerStore.getState().setOpenFileLifecycle(path, "deleted");
    useViewerStore.getState().setOpenFileLifecycle(path, null);
    expect(path in useViewerStore.getState().openFileLifecycleByPath).toBe(false);
  });

  it("deletes the entry when set to active (never stores the default state)", () => {
    useViewerStore.getState().setOpenFileLifecycle(path, "deleted");
    useViewerStore.getState().setOpenFileLifecycle(path, "active");
    expect(path in useViewerStore.getState().openFileLifecycleByPath).toBe(false);
  });

  it("normalizes the key so NFC/NFD and trailing-slash variants coordinate", () => {
    // Write with a redundant slash + NFD; read with the plain NFC form.
    const messy = "/ctx//orgs/o/café.md".normalize("NFD");
    const clean = "/ctx/orgs/o/café.md".normalize("NFC");
    useViewerStore.getState().setOpenFileLifecycle(messy, "temporarily_unavailable");
    expect(useViewerStore.getState().openFileLifecycleByPath[clean]).toBe(
      "temporarily_unavailable",
    );
  });

  it("is a no-op when clearing an already-absent path (stable reference)", () => {
    const before = useViewerStore.getState().openFileLifecycleByPath;
    useViewerStore.getState().setOpenFileLifecycle(path, null);
    expect(useViewerStore.getState().openFileLifecycleByPath).toBe(before);
  });
});
