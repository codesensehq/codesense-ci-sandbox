// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { render, screen } from "@/test-utils";
import {
  AgentAvatar,
  AgentFaceView,
  CrewStack,
  MIN_TAGGED_FACE_PX,
} from "@/features/viewer/components/agent-identity";

const face = { kind: "image" as const, ref: "p:owl", src: "data:owl" };

describe("a face's number tag", () => {
  it("shows the number on a face large enough to read it", () => {
    render(<AgentFaceView face={face} size={20} tag={2} />);
    expect(screen.getByTestId("agent-face-tag")).toHaveTextContent("2");
  });

  it("is left off a face too small to read it, and off an untagged face", () => {
    const { container } = render(
      <>
        <AgentFaceView face={face} size={MIN_TAGGED_FACE_PX - 1} tag={2} />
        <AgentFaceView face={face} size={26} />
      </>,
    );
    expect(container.querySelector("[data-testid=agent-face-tag]")).toBeNull();
  });

  it("sits opposite the role piece on an avatar, and on crew faces", () => {
    const { container } = render(
      <>
        <AgentAvatar
          face={face}
          role={{ piece: "worker", label: "Worker" }}
          pieceTone={null}
          size={26}
          tag={1}
        />
        <CrewStack
          lead={{ face, piece: "manager", label: "Manager", hue: 180, tag: 1 }}
          crew={[{ key: "b", face, name: "b", tag: 2 }]}
          more={0}
          size={15}
        />
      </>,
    );
    const tags = [...container.querySelectorAll("[data-testid=agent-face-tag]")];
    expect(tags.map((t) => t.textContent)).toEqual(["1", "1", "2"]);
    expect(container.querySelector(".ag-avatar .ag-piece")).not.toBeNull();
  });
});
