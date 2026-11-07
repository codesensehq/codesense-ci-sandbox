// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";
import { chatIdForWorkspace } from "./workspace-chat-id";

const BASE_WORKSPACE = {
  id: "story:ws-1",
  kind: "story" as const,
  cwd: "/tmp/w",
  status: "ready" as const,
};

it("uses the bound room id for story, generic and remote workspaces", () => {
  expect(chatIdForWorkspace({ ...BASE_WORKSPACE, id: "sv-x-fix-login", storyId: "SV-X" })).toBe(
    "story:SV-X",
  );
  expect(chatIdForWorkspace({ ...BASE_WORKSPACE, kind: "generic", id: "generic:/tmp/w" })).toBe(
    "generic:/tmp/w",
  );
  expect(
    chatIdForWorkspace({
      ...BASE_WORKSPACE,
      kind: "generic",
      id: "story:SV-X",
      location: { kind: "machine", machineId: "box", requestId: "request" },
    }),
  ).toBe("remote:story:SV-X");
});
