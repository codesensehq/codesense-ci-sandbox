// SPDX-License-Identifier: MIT
import { render, screen } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import type { AgentRuntime } from "@/tauri/commands";

import { RuntimeBadge } from "./agent-bits";

const RUNTIMES: AgentRuntime[] = ["claude", "codex", "opencode"];
const AGENTS_CSS = readFileSync(resolve(__dirname, "../../../styles/agents.css"), "utf-8");

describe("RuntimeBadge", () => {
  it.each(RUNTIMES)("%s renders its icon and carries no raw palette classes", (runtime) => {
    render(<RuntimeBadge runtime={runtime} />);
    const badge = screen.getByTestId("agent-runtime");
    expect(badge.dataset.runtime).toBe(runtime);
    expect(badge.classList.contains("cs-runtime-badge")).toBe(true);
    expect(badge.className).not.toMatch(/\b(?:bg|text)-(?:sky|orange|violet|amber)-\d/);
    expect(badge.querySelector("svg")).not.toBeNull();
  });

  it.each(RUNTIMES)(
    "%s is painted from its --cs-runtime-* tokens, opaque over the surface",
    (runtime) => {
      const rule = new RegExp(
        `\\.cs-runtime-badge\\[data-runtime="${runtime}"\\]\\s*\\{([^}]*)\\}`,
      ).exec(AGENTS_CSS)?.[1];
      expect(rule).toContain(`--rt-bg: var(--cs-runtime-${runtime}-bg)`);
      expect(rule).toContain(`--rt-text: var(--cs-runtime-${runtime}-text)`);
      expect(AGENTS_CSS).toMatch(
        /\.cs-runtime-badge\s*\{[^}]*linear-gradient\(var\(--rt-bg\), var\(--rt-bg\)\) var\(--background\)/,
      );
    },
  );
});
