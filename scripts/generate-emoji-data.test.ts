// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

// @ts-expect-error -- a plain Node ESM build script without type declarations.
import { build, generate, OUTPUT, ROOT, withTone } from "./generate-emoji-data.mjs";

type Emoji = [number, string, string, string, (1 | string[])?];
type Data = { v: number; groups: Array<{ key: string; label: string }>; e: Emoji[] };

const data = (): Data => JSON.parse(readFileSync(`${ROOT}/${OUTPUT}`, "utf8"));

describe("emoji picker data", () => {
  it("is the committed output of the generator", () => {
    expect(() => generate(ROOT, { check: true })).not.toThrow();
    expect(build().equals(readFileSync(`${ROOT}/${OUTPUT}`))).toBe(true);
  });

  it("stays small enough to load lazily", () => {
    expect(readFileSync(`${ROOT}/${OUTPUT}`).length).toBeLessThan(100_000);
  });

  it("has the categories in order, without the skin-tone components", () => {
    const { groups, e } = data();
    expect(groups.map((g) => g.key)).not.toContain("component");
    expect(groups[0].key).toBe("smileys-emotion");
    expect(groups.at(-1)?.key).toBe("flags");
    expect(e.every((row) => row[0] >= 0 && row[0] < groups.length)).toBe(true);
    expect(e.find((row) => row[1] === "👍")?.[2]).toBe("thumbs up");
  });

  it("derives a tone by inserting the modifier after the first code point", () => {
    expect(withTone("👍", 2)).toBe("👍🏽");
    expect(withTone("☝️", 0)).toBe("☝🏻");
    expect(withTone("🧑‍💻", 2)).toBe("🧑🏽‍💻");
    const skinnable = data().e.filter((row) => row[4] === 1);
    expect(skinnable.length).toBeGreaterThan(300);
    expect(skinnable.some((row) => row[1] === "👍")).toBe(true);
    expect(data().e.find((row) => row[1] === "😀")?.[4]).toBeUndefined();
  });

  it("keeps the uniform tones of multi-person emoji as explicit variants", () => {
    const multi = data().e.filter((row) => Array.isArray(row[4]));
    expect(multi.map((row) => row[1])).toEqual(["🧑‍🤝‍🧑", "👩‍❤️‍💋‍👨", "👨‍❤️‍💋‍👨", "👩‍❤️‍💋‍👩", "👩‍❤️‍👨", "👨‍❤️‍👨", "👩‍❤️‍👩"]);
    const holding = multi[0][4] as string[];
    expect(holding).toEqual(["🧑🏻‍🤝‍🧑🏻", "🧑🏼‍🤝‍🧑🏼", "🧑🏽‍🤝‍🧑🏽", "🧑🏾‍🤝‍🧑🏾", "🧑🏿‍🤝‍🧑🏿"]);
    // Everything with tones is either derivable (1) or carries all five variants.
    const toned = data().e.filter((row) => row[4] !== undefined);
    expect(toned.length).toBe(332);
  });
});
