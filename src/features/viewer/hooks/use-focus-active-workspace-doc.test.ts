// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, afterEach } from "vitest";
import { renderHook, cleanup } from "@testing-library/react";
import { useFocusActiveWorkspaceDoc } from "./use-focus-active-workspace-doc";

type Active = Parameters<typeof useFocusActiveWorkspaceDoc>[0];
type Props = { active: Active; cb: (p: string) => void; enabled?: boolean };

const render = (initialProps: Props) =>
  renderHook(
    ({ active, cb, enabled }: Props) => useFocusActiveWorkspaceDoc(active, cb, enabled ?? true),
    { initialProps },
  );

const EPIC: Active = { id: "epic-1", kind: "epic", epicDocPath: "/content/epic-1/README.md" };
const STORY: Active = { id: "story-1", kind: "story", storyFilePath: "/content/story-1.md" };
const STORY_2: Active = { id: "story-2", kind: "story", storyFilePath: "/content/story-2.md" };

afterEach(() => cleanup());

describe("useFocusActiveWorkspaceDoc", () => {
  it("AC-2: focuses the epic doc when the active workspace changes to an epic", () => {
    const onOpen = vi.fn();
    const { rerender } = render({ active: null, cb: onOpen });
    rerender({ active: EPIC, cb: onOpen });
    expect(onOpen).toHaveBeenCalledExactlyOnceWith("/content/epic-1/README.md");
  });

  it("AC-3: focuses the story doc when the active workspace changes to a story", () => {
    const onOpen = vi.fn();
    const { rerender } = render({ active: null, cb: onOpen });
    rerender({ active: STORY, cb: onOpen });
    expect(onOpen).toHaveBeenCalledExactlyOnceWith("/content/story-1.md");
  });

  it("AC-7: does NOT fire for the workspace already active at mount (skip-initial)", () => {
    const onOpen = vi.fn();
    render({ active: EPIC, cb: onOpen });
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("AC-6: is a no-op for non-doc kinds (root / generic)", () => {
    const onOpen = vi.fn();
    const { rerender } = render({ active: null, cb: onOpen });
    rerender({ active: { id: "root-1", kind: "root" }, cb: onOpen });
    rerender({ active: { id: "gen-1", kind: "generic" }, cb: onOpen });
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("AC-8: does not re-fire while the active workspace + its doc are unchanged (additive open / churn)", () => {
    const onOpen1 = vi.fn();
    const onOpen2 = vi.fn();
    const { rerender } = render({ active: null, cb: onOpen1 });
    rerender({ active: STORY, cb: onOpen1 }); // fires once on activation
    // A background/additive open (SW-FX-69) leaves activeWorkspace unchanged; a
    // fresh same-id/same-doc object with a new callback identity must not steal
    // the viewer.
    rerender({ active: { ...STORY }, cb: onOpen2 });
    expect(onOpen1).toHaveBeenCalledOnce();
    expect(onOpen2).not.toHaveBeenCalled();
  });

  it("AC-5: re-focuses on every activation change (A → B → A)", () => {
    const onOpen = vi.fn();
    const { rerender } = render({ active: null, cb: onOpen });
    rerender({ active: STORY, cb: onOpen }); // A
    rerender({ active: STORY_2, cb: onOpen }); // B
    rerender({ active: STORY, cb: onOpen }); // back to A
    expect(onOpen.mock.calls).toEqual([
      ["/content/story-1.md"],
      ["/content/story-2.md"],
      ["/content/story-1.md"],
    ]);
  });

  it("AC-5: re-focuses on return after passing through a doc-less workspace (A → root → A)", () => {
    const onOpen = vi.fn();
    const { rerender } = render({ active: null, cb: onOpen });
    rerender({ active: STORY, cb: onOpen }); // A fires
    rerender({ active: { id: "root-1", kind: "root" }, cb: onOpen }); // doc-less, no fire
    rerender({ active: STORY, cb: onOpen }); // back to A, fires again
    expect(onOpen.mock.calls).toEqual([["/content/story-1.md"], ["/content/story-1.md"]]);
  });

  it("AC-6: switching between two doc-less workspaces (distinct ids) never fires", () => {
    const onOpen = vi.fn();
    const { rerender } = render({ active: null, cb: onOpen });
    rerender({ active: { id: "gen-1", kind: "generic" }, cb: onOpen });
    rerender({ active: { id: "gen-2", kind: "generic" }, cb: onOpen });
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("AC-5: re-focuses when switching to a different workspace that resolves to the same doc (same story, two source repos)", () => {
    const onOpen = vi.fn();
    const { rerender } = render({ active: null, cb: onOpen });
    // Same story projected into two source repos → identical storyFilePath,
    // distinct worktree ids. Switching between them is a genuine activation
    // change and must re-focus even though the resolved doc is unchanged.
    rerender({
      active: { id: "repo1::s", kind: "story", storyFilePath: "/content/s.md" },
      cb: onOpen,
    });
    rerender({
      active: { id: "repo2::s", kind: "story", storyFilePath: "/content/s.md" },
      cb: onOpen,
    });
    expect(onOpen.mock.calls).toEqual([["/content/s.md"], ["/content/s.md"]]);
  });

  it("focuses once the doc path resolves for an already-active workspace (late resolution)", () => {
    const onOpen = vi.fn();
    const { rerender } = render({ active: null, cb: onOpen });
    // Active becomes an epic whose epicDocPath is not yet resolved → no focus.
    rerender({ active: { id: "epic-1", kind: "epic", epicDocPath: undefined }, cb: onOpen });
    expect(onOpen).not.toHaveBeenCalled();
    // The path resolves under the same active workspace → focus fires now.
    rerender({
      active: { id: "epic-1", kind: "epic", epicDocPath: "/content/epic-1/README.md" },
      cb: onOpen,
    });
    expect(onOpen).toHaveBeenCalledExactlyOnceWith("/content/epic-1/README.md");
  });

  it("re-focuses when the active workspace's doc path changes under a stable id (repoint / rename)", () => {
    const onOpen = vi.fn();
    const { rerender } = render({ active: null, cb: onOpen });
    rerender({
      active: { id: "story-1", kind: "story", storyFilePath: "/content/a.md" },
      cb: onOpen,
    });
    rerender({
      active: { id: "story-1", kind: "story", storyFilePath: "/content/a-moved.md" },
      cb: onOpen,
    });
    expect(onOpen.mock.calls).toEqual([["/content/a.md"], ["/content/a-moved.md"]]);
  });

  it("does NOT fire on activation change while disabled (opt-in default off)", () => {
    const onOpen = vi.fn();
    const { rerender } = render({ active: null, cb: onOpen, enabled: false });
    rerender({ active: STORY, cb: onOpen, enabled: false });
    rerender({ active: EPIC, cb: onOpen, enabled: false });
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("enabling mid-session applies from the next activation, not retroactively", () => {
    const onOpen = vi.fn();
    const { rerender } = render({ active: null, cb: onOpen, enabled: false });
    rerender({ active: STORY, cb: onOpen, enabled: false }); // disabled → no fire, key tracked
    rerender({ active: STORY, cb: onOpen, enabled: true }); // just enabled → no retroactive yank
    expect(onOpen).not.toHaveBeenCalled();
    rerender({ active: STORY_2, cb: onOpen, enabled: true }); // next switch → fires
    expect(onOpen).toHaveBeenCalledExactlyOnceWith("/content/story-2.md");
  });

  it("enabling after an in-place doc change (while disabled) does not retroactively yank", () => {
    const onOpen = vi.fn();
    const { rerender } = render({ active: null, cb: onOpen, enabled: false });
    // Same story, doc path changes in place while disabled — the key is tracked.
    rerender({
      active: { id: "story-1", kind: "story", storyFilePath: "/content/a.md" },
      cb: onOpen,
      enabled: false,
    });
    rerender({
      active: { id: "story-1", kind: "story", storyFilePath: "/content/a-moved.md" },
      cb: onOpen,
      enabled: false,
    });
    // Enable with no further activation → the tracked key matches, so no yank.
    rerender({
      active: { id: "story-1", kind: "story", storyFilePath: "/content/a-moved.md" },
      cb: onOpen,
      enabled: true,
    });
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("does not fire when disabled mid-session, and never on toggle with a stable workspace", () => {
    const onOpen = vi.fn();
    const { rerender } = render({ active: STORY, cb: onOpen, enabled: true });
    rerender({ active: STORY, cb: onOpen, enabled: false }); // true→false, same ws
    rerender({ active: STORY, cb: onOpen, enabled: true }); // false→true, same ws
    rerender({ active: STORY, cb: onOpen, enabled: false }); // toggle again, same ws
    expect(onOpen).not.toHaveBeenCalled();
  });
});
