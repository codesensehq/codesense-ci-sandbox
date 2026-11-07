// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import packets from "../../../../tests/fixtures/effective-status-seam.json";
import {
  projectedStatusNodes,
  type AgentListReference,
} from "@/features/viewer/lib/agent-list-projection";
import type { AgentTreeNode } from "@/features/viewer/lib/agent-list-view";

describe("Status arrangement consumes the Rust reference order", () => {
  it("does not sort statuses, unread counts or handles again", () => {
    const refs = packets.initial.order.snapshot.filter(
      (row) => "sessionRef" in row,
    ) as unknown as AgentListReference[];
    const nodes = packets.initial.sessions.snapshot.map((row, i) => ({
      projectedId: `row-${i}`,
      row: { ...row, unread: 100 - i },
      children: [],
      story: null,
      workspace: null,
    })) as unknown as AgentTreeNode[];
    const result = projectedStatusNodes(refs, nodes);
    expect(result.map(({ node }) => node.row.sessionId)).toEqual(
      [...refs].sort((a, b) => a.order - b.order).map((r) => r.sessionRef),
    );
  });
});
