// SPDX-License-Identifier: MIT
import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { FileKindIcon, fileKindOf, type FileKind } from "./file-kind";

describe("fileKindOf — a file's kind from its path", () => {
  it.each<[string, FileKind]>([
    ["/t/README.md", "markdown"],
    ["/t/notes.markdown", "markdown"],
    ["/t/LICENSE", "text"],
    ["/t/run.log", "text"],
    ["/t/spec.pdf", "pdf"],
    ["/t/budget.xlsx", "pdf"],
    ["/t/roadmap.html", "html"],
    ["/t/dashboards/inbox-board.html", "dashboard"],
    ["/t/kickoff.pptx", "slides"],
    ["/t/kickoff.key", "slides"],
    ["/t/flow.excalidraw", "canvas"],
    ["/t/shot.png", "image"],
    ["/t/logo.svg", "image"],
    ["/t/icon.ico", "image"],
    ["/t/demo.mp4", "video"],
    ["/t/voice.mp3", "video"],
    ["/t/index.ts", "code"],
    ["/t/main.rs", "code"],
    ["/t/data.xml", "code"],
    ["/t/data.json", "json"],
    ["/t/team.yaml", "config"],
    ["/t/.env", "config"],
    ["/t/metrics.csv", "table"],
    ["/t/setup.sh", "shell"],
    ["/t/bundle.zip", "archive"],
    ["/t/app.wasm", "archive"],
    ["/t/store.db", "archive"],
    ["/t/.claude/agents/reviewer.md", "agentDef"],
    ["/t/.claude/skills/review/SKILL.md", "agentDef"],
  ])("%s → %s", (path, kind) => {
    expect(fileKindOf(path)).toBe(kind);
  });
});

describe("FileKindIcon", () => {
  it("draws in ink only: no colour but currentColor", () => {
    const kinds: FileKind[] = [
      "markdown",
      "text",
      "pdf",
      "html",
      "dashboard",
      "slides",
      "canvas",
      "image",
      "video",
      "code",
      "json",
      "config",
      "table",
      "shell",
      "archive",
      "folder",
      "folderOpen",
      "epic",
      "repo",
      "agentDef",
    ];
    for (const kind of kinds) {
      const { container, unmount } = render(<FileKindIcon kind={kind} />);
      const svg = container.querySelector("svg")!;
      expect(svg).toHaveAttribute("data-file-kind", kind);
      expect(svg.innerHTML).not.toMatch(/#[0-9a-f]{3,6}|rgb\(/i);
      unmount();
    }
  });

  it("keeps the line weight in CSS px at every size", () => {
    const at = (size: number) => {
      const { container, unmount } = render(<FileKindIcon kind="code" size={size} />);
      const w = Number(container.querySelector("svg")!.getAttribute("stroke-width"));
      unmount();
      return (w * size) / 16;
    };
    expect(at(14)).toBeCloseTo(1.25);
    expect(at(24)).toBeCloseTo(1.15);
    expect(at(10)).toBeCloseTo(1.5);
  });
});
