// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import type { SoftClosedWorkspace, Workspace } from "@/shared/stores/viewer-store";
import { collectInUsePaths } from "./collect-in-use-paths";

const ws = (id: string, cwd: string): Workspace => ({ id, cwd }) as unknown as Workspace;
const shadow = (id: string, cwd: string): SoftClosedWorkspace =>
  ({ workspace: ws(id, cwd) }) as unknown as SoftClosedWorkspace;

describe("collectInUsePaths", () => {
  it("unions open + soft-closed cwds, skips empties, and dedupes", () => {
    const workspaces = {
      "acme/api:default": [ws("a", "/home/_epic/acme/api/E/acme/a"), ws("b", "")],
      "acme/api:team2": [ws("c", "/home/_epic/acme/api/E/acme/a")], // dup of a
    };
    const softClosed = {
      "acme/api:default": {
        d: shadow("d", "/home/_epic/acme/api/E/acme/b"), // minimized epic terminal
        e: shadow("e", ""),
      },
    };

    const paths = collectInUsePaths(workspaces, softClosed).sort();
    expect(paths).toEqual(["/home/_epic/acme/api/E/acme/a", "/home/_epic/acme/api/E/acme/b"]);
  });

  it("returns an empty list when nothing is live", () => {
    expect(collectInUsePaths({}, {})).toEqual([]);
    expect(collectInUsePaths({ "k:t": [ws("a", "")] }, {})).toEqual([]);
  });
});
