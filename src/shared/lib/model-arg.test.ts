// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { readModelArg, setModelArg } from "./model-arg";

describe("readModelArg", () => {
  it("reads --model, --model= and -m", () => {
    expect(readModelArg(["--model", "opus"])).toBe("opus");
    expect(readModelArg(["--verbose", "--model=gpt-5"])).toBe("gpt-5");
    expect(readModelArg(["-m", "sonnet", "--x"])).toBe("sonnet");
  });
  it("is null when no model is set or the value is missing", () => {
    expect(readModelArg([])).toBeNull();
    expect(readModelArg(["--verbose"])).toBeNull();
    expect(readModelArg(["--model"])).toBeNull();
    expect(readModelArg(["--model="])).toBeNull();
  });
  it("reads the first of several", () => {
    expect(readModelArg(["-m", "a", "--model", "b"])).toBe("a");
  });
});

it("can follow native scalar-flag precedence without changing first-match consumers", () => {
  for (const args of [
    ["--model", "first", "--model=last"],
    ["--model=first", "-m", "last"],
    ["-m", "first", "--model", "last"],
  ]) {
    expect(readModelArg(args, "last")).toBe("last");
    expect(readModelArg(args)).toBe("first");
  }
});

describe("setModelArg", () => {
  it("rewrites the one flag in place and keeps every other arg", () => {
    expect(setModelArg(["--a", "--model", "opus", "--b"], "sonnet")).toEqual([
      "--a",
      "--model",
      "sonnet",
      "--b",
    ]);
    expect(setModelArg(["--model=opus", "--b"], "sonnet")).toEqual(["--model=sonnet", "--b"]);
    expect(setModelArg(["-m", "opus"], "sonnet")).toEqual(["-m", "sonnet"]);
  });
  it("appends --model when none is set", () => {
    expect(setModelArg(["--b"], "opus")).toEqual(["--b", "--model", "opus"]);
    expect(setModelArg([], "opus")).toEqual(["--model", "opus"]);
  });
  it("removes the flag and its value for CLI default", () => {
    expect(setModelArg(["--a", "--model", "opus", "--b"], null)).toEqual(["--a", "--b"]);
    expect(setModelArg(["--model=opus"], "")).toEqual([]);
    expect(setModelArg(["-m", "opus", "--b"], null)).toEqual(["--b"]);
  });
  it("drops repeated model flags when removing", () => {
    expect(setModelArg(["-m", "a", "--model=b"], null)).toEqual([]);
  });
  it("leaves args alone when there is nothing to change", () => {
    expect(setModelArg(["--b"], null)).toEqual(["--b"]);
  });
});
