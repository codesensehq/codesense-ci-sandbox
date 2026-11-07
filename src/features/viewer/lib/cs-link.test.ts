// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { dispatchCsLink, handleNoteLinkHref, type CsLinkDispatchDeps } from "./cs-link";

const CONTENT = "/root/content";
const STORY_FILE = `${CONTENT}/_bmad-output/SW-SN-2.md`;

function makeDeps(over: Partial<CsLinkDispatchDeps> = {}): CsLinkDispatchDeps {
  return {
    nodes: [{ storyId: "SW-SN-2", storyFile: STORY_FILE }],
    teamContentPath: CONTENT,
    teamKey: "org:team",
    repoPath: "workspace:org:team:o/r",
    noteExists: () => true,
    setActiveNote: vi.fn(),
    openFile: vi.fn(),
    ...over,
  };
}

describe("dispatchCsLink (SW-SN-9 Task 3 / AC-2)", () => {
  it("cs:note → setActiveNote when the note exists", () => {
    const deps = makeDeps();
    expect(dispatchCsLink({ kind: "note", target: "n1" }, deps)).toBe(true);
    expect(deps.setActiveNote).toHaveBeenCalledWith("n1");
    expect(deps.openFile).not.toHaveBeenCalled();
  });

  it("cs:note → inert (no action) when the note is gone (AC-5)", () => {
    const deps = makeDeps({ noteExists: () => false });
    expect(dispatchCsLink({ kind: "note", target: "gone" }, deps)).toBe(false);
    expect(deps.setActiveNote).not.toHaveBeenCalled();
  });

  it("cs:story → openFile with the reverse-resolved file", () => {
    const deps = makeDeps();
    expect(dispatchCsLink({ kind: "story", target: "SW-SN-2" }, deps)).toBe(true);
    expect(deps.openFile).toHaveBeenCalledWith("org:team", "workspace:org:team:o/r", STORY_FILE);
  });

  it("cs:story → inert when the id is absent from the tree (AC-5)", () => {
    const deps = makeDeps();
    expect(dispatchCsLink({ kind: "story", target: "MISSING" }, deps)).toBe(false);
    expect(deps.openFile).not.toHaveBeenCalled();
  });

  it("cs:file → openFile with the resolved absolute path", () => {
    const deps = makeDeps();
    expect(dispatchCsLink({ kind: "file", target: "_bmad-output/x.md" }, deps)).toBe(true);
    expect(deps.openFile).toHaveBeenCalledWith(
      "org:team",
      "workspace:org:team:o/r",
      `${CONTENT}/_bmad-output/x.md`,
    );
  });

  it("file/story → inert when no workspace is open (repoPath null, AC-5)", () => {
    const deps = makeDeps({ repoPath: null });
    expect(dispatchCsLink({ kind: "file", target: "_bmad-output/x.md" }, deps)).toBe(false);
    expect(deps.openFile).not.toHaveBeenCalled();
  });
});

describe("handleNoteLinkHref (SW-SN-9 Task 1/Task 3 click routing)", () => {
  it("routes a resolvable cs: href to the internal dispatcher → 'activated'", () => {
    const deps = makeDeps();
    const openExternal = vi.fn();
    expect(handleNoteLinkHref("cs:note:n1", deps, openExternal)).toBe("activated");
    expect(deps.setActiveNote).toHaveBeenCalledWith("n1");
    expect(openExternal).not.toHaveBeenCalled();
  });

  it("reports an unresolvable cs: href as 'broken' (AC-5 feedback, no action)", () => {
    const deps = makeDeps();
    const openExternal = vi.fn();
    expect(handleNoteLinkHref("cs:story:MISSING", deps, openExternal)).toBe("broken");
    expect(deps.openFile).not.toHaveBeenCalled();
  });

  it("routes an http(s) href to the external opener → 'external'", () => {
    const deps = makeDeps();
    const openExternal = vi.fn();
    expect(handleNoteLinkHref("https://example.com", deps, openExternal)).toBe("external");
    expect(openExternal).toHaveBeenCalledWith("https://example.com");
  });

  it("leaves an unknown scheme unconsumed → 'ignored'", () => {
    const deps = makeDeps();
    const openExternal = vi.fn();
    expect(handleNoteLinkHref("mailto:a@b.c", deps, openExternal)).toBe("ignored");
    expect(openExternal).not.toHaveBeenCalled();
  });
});
