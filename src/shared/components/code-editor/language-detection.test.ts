// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import {
  detectLanguageId,
  buildLanguageExtension,
  detectLanguage,
  isProseLanguage,
  isCodeLanguage,
  isCodeLanguagePath,
} from "./language-detection";
import { getCodeMirror, _resetCmLoaderForTests } from "./cm-loader";
import type { LanguageId } from "./cm-loader";

describe("detectLanguageId", () => {
  it("never resolves a file name or extension to an Object.prototype member", () => {
    for (const path of [
      "constructor",
      "src/toString",
      "a.constructor",
      "x.toString",
      "__proto__",
      "y.hasOwnProperty",
    ]) {
      expect(detectLanguageId(path)).toBe("plain");
    }
  });

  const cases: Array<[string | undefined, string | undefined, LanguageId]> = [
    // Markdown
    ["README.md", undefined, "markdown"],
    ["docs/intro.markdown", undefined, "markdown"],
    // TypeScript / TSX (both → typescript with jsx: true)
    ["src/foo.ts", undefined, "typescript"],
    ["src/Comp.tsx", undefined, "typescript"],
    // JavaScript / JSX
    ["src/foo.js", undefined, "javascript"],
    ["src/Comp.jsx", undefined, "javascript"],
    ["src/foo.mjs", undefined, "javascript"],
    ["src/foo.cjs", undefined, "javascript"],
    // JSON / YAML / Python
    ["pkg.json", undefined, "json"],
    ["cfg.yaml", undefined, "yaml"],
    ["cfg.yml", undefined, "yaml"],
    ["script.py", undefined, "python"],
    // Shell variants
    ["run.sh", undefined, "shell"],
    ["run.bash", undefined, "shell"],
    ["run.zsh", undefined, "shell"],
    // Rust
    ["src/main.rs", undefined, "rust"],
    // SW-VE-2: XML (legacy mode)
    ["pom.xml", undefined, "xml"],
    ["config.XML", undefined, "xml"],
    // Plain fallback
    ["LICENSE", undefined, "plain"],
    ["unknown.xyz", undefined, "plain"],
    [undefined, undefined, "plain"],
    // Case insensitivity
    ["README.MD", undefined, "markdown"],
    ["FOO.TS", undefined, "typescript"],
  ];

  // SW-VE-2 AC-2: XML must NOT fall through to plain (the whole point of the
  // legacy-mode addition — a .xml file highlights).
  it("SW-VE-2: .xml resolves to the xml language, never plain", () => {
    expect(detectLanguageId("pom.xml")).toBe("xml");
    expect(detectLanguageId("pom.xml")).not.toBe("plain");
  });

  for (const [path, content, expected] of cases) {
    it(`detects ${expected} for path=${JSON.stringify(path)} content=${content ?? "(none)"}`, () => {
      expect(detectLanguageId(path, content)).toBe(expected);
    });
  }

  describe("SW-VE-8 prose-vs-code classification", () => {
    it("isProseLanguage: only markdown + plain are prose", () => {
      expect(isProseLanguage("markdown")).toBe(true);
      expect(isProseLanguage("plain")).toBe(true);
      expect(isProseLanguage("typescript")).toBe(false);
      expect(isProseLanguage("json")).toBe(false);
      expect(isProseLanguage("xml")).toBe(false);
    });

    it("isCodeLanguage is the inverse of isProseLanguage", () => {
      const ids: LanguageId[] = [
        "markdown",
        "plain",
        "typescript",
        "javascript",
        "json",
        "yaml",
        "python",
        "shell",
        "rust",
        "xml",
      ];
      for (const id of ids) expect(isCodeLanguage(id)).toBe(!isProseLanguage(id));
    });

    it("isCodeLanguagePath: prose paths (markdown/text) are NOT code", () => {
      expect(isCodeLanguagePath("README.md")).toBe(false);
      expect(isCodeLanguagePath("notes.markdown")).toBe(false);
      expect(isCodeLanguagePath("CHANGELOG.txt")).toBe(false);
      expect(isCodeLanguagePath("LICENSE")).toBe(false); // extensionless → plain
    });

    it("isCodeLanguagePath: code + markup paths ARE code (html special-cased)", () => {
      expect(isCodeLanguagePath("main.rs")).toBe(true);
      expect(isCodeLanguagePath("config.yaml")).toBe(true);
      expect(isCodeLanguagePath("data.json")).toBe(true);
      expect(isCodeLanguagePath("pom.xml")).toBe(true);
      // html detects as `plain` via detectLanguageId but is markup → code-side.
      expect(isCodeLanguagePath("dashboard.html")).toBe(true);
      expect(isCodeLanguagePath("index.htm")).toBe(true);
    });
  });

  describe("shebang overrides extension", () => {
    it("treats #!/bin/bash as shell even with .txt extension", () => {
      expect(detectLanguageId("setup.txt", "#!/bin/bash\necho hi")).toBe("shell");
    });

    it("treats #!/usr/bin/env bash as shell", () => {
      expect(detectLanguageId("setup.txt", "#!/usr/bin/env bash\n")).toBe("shell");
    });

    it("treats #!/usr/bin/env zsh as shell", () => {
      expect(detectLanguageId("setup.txt", "#!/usr/bin/env zsh\n")).toBe("shell");
    });

    it("treats #!/usr/bin/env -S zsh as shell", () => {
      expect(detectLanguageId("setup.txt", "#!/usr/bin/env -S zsh\n")).toBe("shell");
    });

    it("treats #!/bin/sh as shell", () => {
      expect(detectLanguageId("setup.txt", "#!/bin/sh\n")).toBe("shell");
    });

    // A-4 patch: explicit coverage for the shells added to L-7's whitelist.
    it("treats #!/bin/dash as shell", () => {
      expect(detectLanguageId("setup.txt", "#!/bin/dash\n")).toBe("shell");
    });

    it("treats #!/bin/csh as shell", () => {
      expect(detectLanguageId("setup.txt", "#!/bin/csh\n")).toBe("shell");
    });

    it("treats #!/bin/tcsh as shell", () => {
      expect(detectLanguageId("setup.txt", "#!/bin/tcsh\n")).toBe("shell");
    });

    it("treats #!/bin/ksh as shell", () => {
      expect(detectLanguageId("setup.txt", "#!/bin/ksh\n")).toBe("shell");
    });

    it("treats #!/usr/bin/env fish as shell", () => {
      expect(detectLanguageId("setup.txt", "#!/usr/bin/env fish\n")).toBe("shell");
    });

    it("treats #!/usr/bin/env nu as shell (Nushell)", () => {
      expect(detectLanguageId("setup.txt", "#!/usr/bin/env nu\n")).toBe("shell");
    });

    it("does NOT misfire on non-shebang first line", () => {
      expect(detectLanguageId("README.md", "# Hello\n")).toBe("markdown");
    });

    it("does NOT misfire on python shebang (sticks to .py extension)", () => {
      expect(detectLanguageId("script.py", "#!/usr/bin/env python3\nprint('hi')")).toBe("python");
    });
  });
});

describe("buildLanguageExtension + detectLanguage", () => {
  // E2-L1 / A-5 patch: the iter-1 file had a local `beforeAll()` shim that
  // was a plain function (not Vitest's hook), so the reset ran once at
  // module load instead of per test. Wire the real beforeEach now.
  beforeEach(() => {
    _resetCmLoaderForTests();
  });

  it("returns null (plain text) for a language with no factory instead of throwing", () => {
    const cm = { languages: {} } as unknown as Parameters<typeof buildLanguageExtension>[0];
    expect(buildLanguageExtension(cm, "typescript")).toBeNull();
  });

  it("returns null for plain", async () => {
    const cm = await getCodeMirror();
    expect(buildLanguageExtension(cm, "plain")).toBeNull();
  });

  it("returns a non-null Extension for every concrete language", async () => {
    const cm = await getCodeMirror();
    const langs: LanguageId[] = [
      "markdown",
      "typescript",
      "javascript",
      "json",
      "yaml",
      "python",
      "shell",
      "rust",
      "xml",
    ];
    for (const lang of langs) {
      const ext = buildLanguageExtension(cm, lang);
      expect(ext, `extension for ${lang}`).toBeDefined();
      expect(ext, `extension for ${lang}`).not.toBeNull();
    }
  });

  it("detectLanguage composes detectLanguageId + buildLanguageExtension", async () => {
    const cm = await getCodeMirror();
    expect(detectLanguage(cm, "README.md")).not.toBeNull();
    expect(detectLanguage(cm, "LICENSE")).toBeNull();
    // Shebang path
    expect(detectLanguage(cm, "setup.txt", "#!/bin/bash\n")).not.toBeNull();
  });
});

describe("worktree editor languages", () => {
  it.each<[string, LanguageId]>([
    ["styles/app.css", "css"],
    ["styles/app.scss", "css"],
    ["index.html", "html"],
    ["cmd/main.go", "go"],
    ["Cargo.toml", "toml"],
    ["Cargo.lock", "toml"],
    ["db/schema.sql", "sql"],
    ["src/io.c", "c"],
    ["src/io.h", "c"],
    ["src/io.cpp", "cpp"],
    ["App.java", "java"],
    ["build.gradle.kts", "kotlin"],
    ["Program.cs", "csharp"],
    ["App.swift", "swift"],
    ["Gemfile", "ruby"],
    ["lib/x.rb", "ruby"],
    ["init.lua", "lua"],
    ["Dockerfile", "dockerfile"],
    ["deploy/Dockerfile.prod", "dockerfile"],
    ["web.dockerfile", "dockerfile"],
    ["fix.patch", "diff"],
    [".env", "properties"],
    ["setup.cfg", "properties"],
    ["notes.txt", "plain"],
    ["LICENSE", "plain"],
  ])("%s → %s", (path, expected) => {
    expect(detectLanguageId(path)).toBe(expected);
  });

  it("labels every language for the status bar", async () => {
    const { languageLabel } = await import("./language-detection");
    expect(languageLabel("cpp")).toBe("C++");
    expect(languageLabel("plain")).toBe("Plain Text");
  });

  it("builds an extension for every new language", async () => {
    _resetCmLoaderForTests();
    const cm = await getCodeMirror();
    const ids: LanguageId[] = [
      "css",
      "html",
      "go",
      "toml",
      "sql",
      "c",
      "cpp",
      "java",
      "kotlin",
      "csharp",
      "swift",
      "ruby",
      "lua",
      "dockerfile",
      "diff",
      "properties",
    ];
    for (const id of ids) expect(buildLanguageExtension(cm, id)).not.toBeNull();
  });
});
