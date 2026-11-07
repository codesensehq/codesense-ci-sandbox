// SPDX-License-Identifier: MIT
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

/** The Agents tab draws from the app's tokens only: no palette classes, no hex colours. */
const DIRS = [
  "src/features/agents/components",
  "src/features/agents/lib",
  "src/features/routines/components",
];
const FORBIDDEN = /\b(?:amber|emerald|sky)-\d|#[0-9a-fA-F]{3,8}\b/;
/** The type scale is 11, 12, 13 and 14 (18 to 20 for names and titles): no half pixels, no rem sizes. */
const OFF_SCALE = /\b1[12]\.5px|text-\[0?\.\d+rem\]/;

function sources(dir: string): string[] {
  return readdirSync(dir)
    .filter((f) => /\.(tsx?|css)$/.test(f) && !/\.test\.tsx?$/.test(f))
    .map((f) => join(dir, f));
}

describe("the Agents tab's palette", () => {
  it.each([...DIRS.flatMap(sources), "src/styles/agent-launch.css"])(
    "%s uses no palette class, hex colour or off-scale type size",
    (file) => {
      const offending = readFileSync(file, "utf8")
        .split("\n")
        .map((text, i) => ({ text, line: i + 1 }))
        // A `#` that opens a heading or an anchor in prose is not a colour: only code lines count.
        .filter(
          ({ text }) =>
            (FORBIDDEN.test(text) || OFF_SCALE.test(text)) && !/^\s*(\/\/|\*|\/\*)/.test(text),
        );
      expect(offending).toEqual([]);
    },
  );
});
