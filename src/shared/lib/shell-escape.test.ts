// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { shellEscape, stripControlChars } from "./shell-escape";

describe("shellEscape", () => {
  it("escapes spaces", () => {
    expect(shellEscape("/path/to/my file.txt")).toBe("/path/to/my\\ file.txt");
  });

  it("escapes parentheses", () => {
    expect(shellEscape("/path/(foo).txt")).toBe("/path/\\(foo\\).txt");
  });

  it("escapes single and double quotes", () => {
    expect(shellEscape(`/path/"it's".txt`)).toBe(`/path/\\"it\\'s\\".txt`);
  });

  it("escapes backticks and dollar signs", () => {
    expect(shellEscape("/path/$HOME/`cmd`")).toBe("/path/\\$HOME/\\`cmd\\`");
  });

  it("escapes ampersand pipe semicolon", () => {
    expect(shellEscape("a&b|c;d")).toBe("a\\&b\\|c\\;d");
  });

  it("escapes glob characters", () => {
    expect(shellEscape("*.txt")).toBe("\\*.txt");
    expect(shellEscape("file[0].txt")).toBe("file\\[0\\].txt");
    expect(shellEscape("file?.txt")).toBe("file\\?.txt");
  });

  it("escapes braces tilde hash", () => {
    expect(shellEscape("{a,b}")).toBe("\\{a,b\\}");
    expect(shellEscape("~/file")).toBe("\\~/file");
    expect(shellEscape("#comment")).toBe("\\#comment");
  });

  it("escapes redirects and backslash", () => {
    expect(shellEscape("a<b>c")).toBe("a\\<b\\>c");
    expect(shellEscape("path\\file")).toBe("path\\\\file");
  });

  it("escapes exclamation mark", () => {
    expect(shellEscape("hello!")).toBe("hello\\!");
  });

  it("returns empty string unchanged", () => {
    expect(shellEscape("")).toBe("");
  });

  it("leaves normal paths unchanged", () => {
    expect(shellEscape("/usr/local/bin/node")).toBe("/usr/local/bin/node");
  });

  it("handles path with mixed special characters", () => {
    expect(shellEscape("/Users/me/My Files (2)/doc's.txt")).toBe(
      "/Users/me/My\\ Files\\ \\(2\\)/doc\\'s.txt",
    );
  });
});

describe("stripControlChars", () => {
  it("strips null byte", () => {
    expect(stripControlChars("a\x00b")).toBe("ab");
  });

  it("strips carriage return and newline", () => {
    expect(stripControlChars("a\r\nb")).toBe("ab");
  });

  it("strips Ctrl+C (0x03)", () => {
    expect(stripControlChars("a\x03b")).toBe("ab");
  });

  it("strips DEL (0x7f)", () => {
    expect(stripControlChars("a\x7fb")).toBe("ab");
  });

  it("strips all control chars 0x00-0x1f", () => {
    let input = "";
    for (let i = 0; i <= 0x1f; i++) {
      input += String.fromCharCode(i);
    }
    expect(stripControlChars(input)).toBe("");
  });

  it("preserves normal text and unicode", () => {
    expect(stripControlChars("hello world 日本語")).toBe("hello world 日本語");
  });

  it("returns empty string unchanged", () => {
    expect(stripControlChars("")).toBe("");
  });

  it("handles path with embedded control chars", () => {
    expect(stripControlChars("/path/evil\rname\nfile\x03.txt")).toBe("/path/evilnamefile.txt");
  });
});
