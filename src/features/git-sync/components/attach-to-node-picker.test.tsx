// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { Node } from "@xyflow/react";

import { buildAttachTargets, AttachToNodePicker, type AttachTarget } from "./attach-to-node-picker";

function storyNode(id: string, data: Record<string, unknown>): Node {
  return { id, type: "story", position: { x: 0, y: 0 }, data } as Node;
}
function groupNode(id: string, data: Record<string, unknown>): Node {
  return { id, type: "group", position: { x: 0, y: 0 }, data } as Node;
}

describe("buildAttachTargets", () => {
  it("maps a story node to its own .md, labeled by story_id", () => {
    const nodes = [
      storyNode("s1", {
        title: "Node-level publish",
        storyId: "SW-GS-8",
        storyFile: "/ctx/git-sync/SW-GS-8-node-level-publish.md",
      }),
    ];
    expect(buildAttachTargets(nodes)).toEqual<AttachTarget[]>([
      {
        mdPath: "/ctx/git-sync/SW-GS-8-node-level-publish.md",
        label: "SW-GS-8",
        sublabel: "SW-GS-8-node-level-publish.md",
      },
    ]);
  });

  it("falls back to the title when a story has no story_id", () => {
    const nodes = [storyNode("s1", { title: "Loose Story", storyFile: "/ctx/x/loose.md" })];
    expect(buildAttachTargets(nodes)[0].label).toBe("Loose Story");
  });

  it("maps an epic node to its README, searchable by dir name", () => {
    const nodes = [
      groupNode("g1", { title: "Workflows", mainDocFilePath: "/ctx/seamless/workflows/README.md" }),
    ];
    expect(buildAttachTargets(nodes)).toEqual<AttachTarget[]>([
      { mdPath: "/ctx/seamless/workflows/README.md", label: "Workflows", sublabel: "workflows" },
    ]);
  });

  it("includes BOTH stories and epics", () => {
    const nodes = [
      storyNode("s1", { title: "S", storyId: "SW-1", storyFile: "/ctx/a/SW-1.md" }),
      groupNode("g1", { title: "Epic", mainDocFilePath: "/ctx/a/README.md" }),
    ];
    const kinds = buildAttachTargets(nodes).map((t) => t.mdPath);
    expect(kinds).toEqual(["/ctx/a/SW-1.md", "/ctx/a/README.md"]);
  });

  it("skips a story with no storyFile and an epic with no mainDocFilePath (nothing to write)", () => {
    const nodes = [storyNode("s1", { title: "No file" }), groupNode("g1", { title: "No readme" })];
    expect(buildAttachTargets(nodes)).toEqual([]);
  });

  it("falls back to the title for a blank (empty-string) story_id", () => {
    const nodes = [storyNode("s1", { title: "Real Title", storyId: "", storyFile: "/ctx/a/x.md" })];
    expect(buildAttachTargets(nodes)[0].label).toBe("Real Title");
  });

  it("dedups nodes that resolve to the same .md (first wins, no key collision)", () => {
    const nodes = [
      groupNode("g1", { title: "Epic A", mainDocFilePath: "/ctx/a/README.md" }),
      groupNode("g2", { title: "Epic A dup", mainDocFilePath: "/ctx/a/README.md" }),
    ];
    const targets = buildAttachTargets(nodes);
    expect(targets).toHaveLength(1);
    expect(targets[0].label).toBe("Epic A");
  });

  it("does not drop the last dir char for a root-level epic README (no false 'epic' collapse)", () => {
    const nodes = [groupNode("g1", { title: "Root Epic", mainDocFilePath: "/README.md" })];
    // dir is "" (not "/README.m"), so the sublabel falls back cleanly to "epic".
    expect(buildAttachTargets(nodes)[0].sublabel).toBe("epic");
  });
});

describe("AttachToNodePicker", () => {
  const targets: AttachTarget[] = [
    { mdPath: "/ctx/a/SW-1.md", label: "SW-1", sublabel: "SW-1.md" },
    { mdPath: "/ctx/a/README.md", label: "My Epic", sublabel: "a" },
  ];

  it("renders both story and epic candidates", () => {
    render(<AttachToNodePicker targets={targets} onSelect={vi.fn()} onClose={vi.fn()} />);
    expect(screen.getByText("SW-1")).toBeInTheDocument();
    expect(screen.getByText("My Epic")).toBeInTheDocument();
  });

  it("resolves a click to the full AttachTarget", async () => {
    const onSelect = vi.fn();
    render(<AttachToNodePicker targets={targets} onSelect={onSelect} onClose={vi.fn()} />);

    await userEvent.click(screen.getByText("My Epic"));
    expect(onSelect).toHaveBeenCalledWith(targets[1]);
  });

  it("finds an epic by typing its displayed title (not just its dir-slug sublabel)", async () => {
    // "My Epic" is the label; the sublabel is the dir slug "a". Typing the
    // visible title must surface it — the picker searches label + sublabel.
    render(<AttachToNodePicker targets={targets} onSelect={vi.fn()} onClose={vi.fn()} />);

    await userEvent.type(screen.getByPlaceholderText("Attach to a story or epic..."), "My Epic");
    expect(screen.getByText("My Epic")).toBeInTheDocument();
    // The unrelated story is filtered out by the query.
    expect(screen.queryByText("SW-1")).toBeNull();
  });
});
