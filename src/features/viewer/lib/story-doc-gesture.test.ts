// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { canOpenWorkspaceDoc, openWorkspaceDoc, workspaceDocPath } from "./story-doc-gesture";

const storyWs = { kind: "story" as const, storyFilePath: "/r/s.md" };
const epicWs = { kind: "epic" as const, epicDocPath: "/r/epic/epic.md" };

describe("story-doc-gesture", () => {
  describe("workspaceDocPath", () => {
    it("resolves a story to its storyFilePath and an epic to its epicDocPath", () => {
      expect(workspaceDocPath(storyWs)).toBe("/r/s.md");
      expect(workspaceDocPath(epicWs)).toBe("/r/epic/epic.md");
    });
    it("is undefined for a root or a record missing its path", () => {
      expect(workspaceDocPath({ kind: "root", storyFilePath: "/r/s.md" })).toBeUndefined();
      expect(workspaceDocPath({ kind: "epic", epicDocPath: undefined })).toBeUndefined();
    });
  });

  describe("canOpenWorkspaceDoc", () => {
    it("true for a story OR an epic workspace with a path and a handler", () => {
      expect(canOpenWorkspaceDoc(storyWs, vi.fn())).toBe(true);
      expect(canOpenWorkspaceDoc(epicWs, vi.fn())).toBe(true);
    });
    it("false for a root, a doc-less tile, or a missing handler", () => {
      expect(canOpenWorkspaceDoc({ kind: "root", storyFilePath: "/r/s.md" }, vi.fn())).toBe(false);
      expect(canOpenWorkspaceDoc({ kind: "epic", epicDocPath: undefined }, vi.fn())).toBe(false);
      expect(canOpenWorkspaceDoc(storyWs, undefined)).toBe(false);
    });
  });

  describe("openWorkspaceDoc", () => {
    it("fires onOpenStoryDoc with the story path and returns true", () => {
      const fn = vi.fn();
      expect(openWorkspaceDoc(storyWs, fn)).toBe(true);
      expect(fn).toHaveBeenCalledWith("/r/s.md");
    });
    it("fires onOpenStoryDoc with the epic doc path and returns true (SW-EW-35)", () => {
      const fn = vi.fn();
      expect(openWorkspaceDoc(epicWs, fn)).toBe(true);
      expect(fn).toHaveBeenCalledWith("/r/epic/epic.md");
    });
    it("does nothing and returns false for a doc-less workspace", () => {
      const fn = vi.fn();
      expect(openWorkspaceDoc({ kind: "epic", epicDocPath: undefined }, fn)).toBe(false);
      expect(fn).not.toHaveBeenCalled();
    });
    it("returns false when no handler is provided", () => {
      expect(openWorkspaceDoc(storyWs, undefined)).toBe(false);
    });
  });
});
