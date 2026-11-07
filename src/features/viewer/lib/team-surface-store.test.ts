// SPDX-License-Identifier: MIT
import { afterEach, expect, it } from "vitest";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { publishTeamTree, useTeamSurfaceStore } from "./team-surface-store";

afterEach(() => useTeamSurfaceStore.setState({ trees: {} }));

it("keeps the last loaded tree across a loading gap, then accepts its replacement", () => {
  const initial = { epics: [] } as unknown as DerivedPipelineTree;
  const next = { epics: [] } as unknown as DerivedPipelineTree;
  publishTeamTree("acme:alpha", initial);
  publishTeamTree("acme:alpha", null);
  expect(useTeamSurfaceStore.getState().trees["acme:alpha"]).toBe(initial);
  publishTeamTree("acme:alpha", next);
  expect(useTeamSurfaceStore.getState().trees["acme:alpha"]).toBe(next);
});
