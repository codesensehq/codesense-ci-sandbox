// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { JSDOM } from "jsdom";
import { readFileSync } from "node:fs";

// @ts-expect-error -- a plain Node ESM build script without type declarations.
import { buildManifest } from "./generate-primitives-reference.mjs";

type Manifest = {
  schema: number;
  generatedAt: string;
  counts: Record<string, number>;
  invoke: Array<{
    name: string;
    category: string;
    write: boolean;
    audited: boolean;
    readOnlyDocsSafe: boolean;
    description: string;
    args: unknown;
    returns: unknown;
    sourceRef: string;
    starter?: true;
    examples?: Array<{ args?: unknown; result?: unknown }>;
  }>;
  kit: Array<{
    name: string;
    category: string;
    signature: string;
    wraps: string;
    description: string;
    example: string;
    sourceRef: string;
    starter?: true;
  }>;
  classes: Array<{
    name: string;
    category: string;
    description: string;
    appliedBy: string;
    tokensUsed: string[];
    example: string;
    sourceRef: string;
  }>;
  tokens: Array<{
    name: string;
    role: string;
    swatchType: string;
    fallbackChain: string[];
    sourceRef: string;
  }>;
  events: Array<{
    name: string;
    direction: "parent-to-doc" | "doc-to-parent" | "reply";
    payload: string;
    timing: string;
    description: string;
    example: string;
    sourceRef: string;
  }>;
  contexts: Array<{
    name: string;
    category: string;
    route: string;
    sandbox: string;
    bridgeFull: boolean;
    snapshotAvailable: boolean;
    detection: string;
    description: string;
    features: Record<string, unknown>;
    sourceRef: string;
  }>;
};

// The build is driven by on-disk sources; this test asserts the SHAPE of the
// output + the invariants the explorer relies on. New verbs / tokens / events
// landing in those sources only extend the arrays — the invariants hold.

describe("generate-primitives-reference manifest shape", () => {
  const { doc, warnings } = buildManifest() as {
    doc: Manifest;
    warnings: string[];
  };

  it("emits the six keyed sections with matching counts (AC-1..AC-6)", () => {
    expect(doc.schema).toBe(1);
    expect(doc.generatedAt).toBe("deterministic");
    expect(Array.isArray(doc.invoke)).toBe(true);
    expect(Array.isArray(doc.kit)).toBe(true);
    expect(Array.isArray(doc.classes)).toBe(true);
    expect(Array.isArray(doc.tokens)).toBe(true);
    expect(Array.isArray(doc.events)).toBe(true);
    expect(Array.isArray(doc.contexts)).toBe(true);
    expect(doc.counts.invoke).toBe(doc.invoke.length);
    expect(doc.counts.kit).toBe(doc.kit.length);
    expect(doc.counts.classes).toBe(doc.classes.length);
    expect(doc.counts.tokens).toBe(doc.tokens.length);
    expect(doc.counts.events).toBe(doc.events.length);
    expect(doc.counts.contexts).toBe(doc.contexts.length);
  });

  it("every invoke entry carries a readOnlyDocsSafe boolean (AC-8)", () => {
    expect(doc.invoke.length).toBeGreaterThan(0);
    for (const v of doc.invoke) {
      expect(typeof v.readOnlyDocsSafe).toBe("boolean");
    }
  });

  it("READ-class verbs are readOnlyDocsSafe:true, WRITE-class are false (AC-8)", () => {
    // The read lanes in html-bridge.ts are READ_VERBS + READ_ACTION_VERBS.
    // Everything else (METADATA / SPAWN / SEND / MANAGEMENT) is a write and
    // must refuse on the docs org.
    const read = doc.invoke.filter((v) => v.category === "READ" || v.category === "READ_ACTION");
    const write = doc.invoke.filter((v) =>
      ["METADATA", "SPAWN", "SEND", "MANAGEMENT"].includes(v.category),
    );
    expect(read.length).toBeGreaterThan(0);
    expect(write.length).toBeGreaterThan(0);
    for (const v of read) expect(v.readOnlyDocsSafe).toBe(true);
    for (const v of write) expect(v.readOnlyDocsSafe).toBe(false);
  });

  it("invoke carries the brief's starter primitives pinned", () => {
    const names = new Set(doc.invoke.map((v) => v.name));
    // Core READ staples that every dashboard reaches for.
    expect(names.has("queryContext")).toBe(true);
    expect(names.has("context")).toBe(true);
    expect(names.has("sessions")).toBe(true);
    expect(names.has("agents")).toBe(true);
    // At least one entry carries starter:true (the brief's "Start here" bucket).
    expect(doc.invoke.some((v) => v.starter === true)).toBe(true);
  });

  it("uses the registry's audit classification for every invoke verb", () => {
    const registry = JSON.parse(
      readFileSync("src-tauri/src/verbs/registry.generated.json", "utf8"),
    );
    for (const entry of doc.invoke) {
      expect(entry.audited).toBe(
        registry.find((verb: { name: string }) => verb.name === entry.name).audited,
      );
    }
  });

  it("labels newly included record writes as actions in the explorer", () => {
    expect(doc.kit.find((entry) => entry.name === "codesense.records.create")?.category).toBe(
      "action",
    );
    expect(doc.kit.find((entry) => entry.name === "codesense.records.list")?.category).toBe(
      "data-read",
    );
  });

  it("every invoke entry identifies the authoritative registry", () => {
    for (const v of doc.invoke) {
      expect(v.sourceRef).toBe("src-tauri/src/verbs/registry.generated.json");
    }
  });

  it("kit exports the core bridge + listener catalog (AC-2)", () => {
    const names = new Set(doc.kit.map((k) => k.name));
    expect(names.has("codesense.invoke")).toBe(true);
    expect(names.has("codesense.subscribeContext")).toBe(true);
    expect(names.has("codesense.openFile")).toBe(true);
    expect(names.has("codesense.onTheme")).toBe(true);
    expect(names.has("codesense.setBadge")).toBe(true);
    expect(names.has("codesense.snapshot")).toBe(true);
    // Every kit entry carries a signature + example — the explorer's detail
    // pane renders both verbatim.
    for (const k of doc.kit) {
      expect(k.signature.length).toBeGreaterThan(0);
      expect(k.example.length).toBeGreaterThan(0);
    }
  });

  it("classes carries each cs-* class with a tokens-used list (AC-3)", () => {
    for (const c of doc.classes) {
      expect(c.name).toMatch(/^cs-[a-z0-9-]+$/);
      expect(Array.isArray(c.tokensUsed)).toBe(true);
      for (const t of c.tokensUsed) expect(t).toMatch(/^--cs-[a-z0-9-]+$/);
    }
    // The shell families from the inventory are all present.
    const names = new Set(doc.classes.map((c) => c.name));
    expect(names.has("cs-page")).toBe(true);
    expect(names.has("cs-topnav")).toBe(true);
    expect(names.has("cs-main")).toBe(true);
    expect(names.has("cs-tab-item")).toBe(true);
    expect(names.has("cs-tab-item-active")).toBe(true);
  });

  it("tokens include every TOKEN_SOURCES entry + synthesized font-mono + color-scheme", () => {
    const names = new Set(doc.tokens.map((t) => t.name));
    expect(names.has("--cs-bg")).toBe(true);
    expect(names.has("--cs-fg")).toBe(true);
    expect(names.has("--cs-accent")).toBe(true);
    expect(names.has("--cs-link")).toBe(true);
    expect(names.has("--cs-font-sans")).toBe(true);
    expect(names.has("--cs-font-mono")).toBe(true);
    expect(names.has("--cs-color-scheme")).toBe(true);
    // Every token has a fallback chain the Tokens tab renders.
    for (const t of doc.tokens) {
      expect(Array.isArray(t.fallbackChain)).toBe(true);
      expect(t.fallbackChain.length).toBeGreaterThan(0);
    }
  });

  it("events include every postMessage type the host emits + the invoke RPC (AC-4)", () => {
    const names = new Set(doc.events.map((e) => e.name));
    // Parent → doc (host-pushed nudges).
    expect(names.has("codesense:theme")).toBe(true);
    expect(names.has("codesense:visibility")).toBe(true);
    expect(names.has("codesense:context-changed")).toBe(true);
    expect(names.has("codesense:inbox-changed")).toBe(true);
    expect(names.has("codesense:agents-changed")).toBe(true);
    expect(names.has("codesense:routines-changed")).toBe(true);
    expect(names.has("codesense:machines-changed")).toBe(true);
    expect(names.has("codesense:worktrees-changed")).toBe(true);
    expect(names.has("codesense:delta")).toBe(true);
    expect(names.has("codesense:stale")).toBe(true);
    expect(names.has("codesense:resync")).toBe(true);
    expect(names.has("codesense:snapshot-request")).toBe(true);
    // Doc → parent (RPC + navigation).
    expect(names.has("codesense:invoke")).toBe(true);
    expect(names.has("codesense:ready")).toBe(true);
    // Correlated replies.
    expect(names.has("codesense:invoke-result")).toBe(true);
    expect(names.has("codesense:snapshot-result")).toBe(true);
    // Every entry carries a direction.
    for (const e of doc.events) {
      expect(["parent-to-doc", "doc-to-parent", "reply"]).toContain(e.direction);
    }
  });

  it("contexts documents centre / pane / message + baseline with detection snippets (AC-5)", () => {
    const names = doc.contexts.map((c) => c.name);
    expect(names).toContain("centre-tab");
    expect(names).toContain("right-pane");
    expect(names).toContain("inbox-message");
    expect(names).toContain("baseline");
    for (const c of doc.contexts) {
      expect(c.detection.length).toBeGreaterThan(0);
    }
    const baseline = doc.contexts.find((c) => c.name === "baseline");
    expect(baseline?.bridgeFull).toBe(false);
  });

  it("warns loudly when a bridge verb has no INVOKE_META entry (automation gate)", () => {
    // The warnings array is a developer-facing stream; empty today means every
    // allowlist verb carries a described entry. If this test fails after a new
    // verb lands, add it to the INVOKE_META table (the warning string names
    // which generator table to edit).
    const unknownVerbWarnings = warnings.filter((w) => w.includes("INVOKE_META entry"));
    expect(unknownVerbWarnings).toEqual([]);
  });

  it("documents agent-list action arguments and focus preservation", () => {
    const verb = (name: string) => doc.invoke.find((entry) => entry.name === name)!;
    expect(verb("moveWorkspace").args).toMatchObject({
      required: ["teamKey", "workspaceId"],
      oneOf: [{ required: ["before"] }, { required: ["after"] }],
    });
    expect(verb("launchUnder").args).toMatchObject({
      required: ["handle"],
      properties: { task: { type: "string" } },
    });
    expect(verb("openAgentInbox").args).toMatchObject({ required: ["handle"] });
    expect(verb("gotoAgent").args).toMatchObject({
      properties: { preserveFocus: { type: "boolean" } },
    });
  });

  it("warns loudly when a kit method is listed but missing from the viewer shim", () => {
    const stale = warnings.filter((w) => w.includes("not found in viewer.rs shim"));
    expect(stale).toEqual([]);
  });
});

describe("SDK signature ownership", () => {
  it("lists SDK and action members in the normal Kit browser", async () => {
    const html = readFileSync(
      "codesense-docs/getting-started/content/primitives-explorer.html",
      "utf8",
    );
    const dom = new JSDOM(html, {
      url: "https://docs.test/primitives-explorer.html#kit",
      runScripts: "outside-only",
    });
    try {
      dom.window.fetch = vi
        .fn()
        .mockResolvedValue({ ok: true, json: async () => buildManifest().doc });
      for (const script of dom.window.document.querySelectorAll("script:not([src])"))
        dom.window.eval(script.textContent!);
      dom.window.document.dispatchEvent(new dom.window.Event("DOMContentLoaded"));
      await vi.waitFor(() =>
        expect(dom.window.document.querySelector("#cs-pe-list .cs-pe-row")).not.toBeNull(),
      );
      for (const name of ["codesense.has", "codesense.records.create"]) {
        expect(
          dom.window.document.querySelector(`#cs-pe-list [data-name="${name}"]`),
          `${name} must be discoverable in Kit`,
        ).not.toBeNull();
      }
    } finally {
      dom.window.close();
    }
  });

  it("shows only Surface signatures for shadowed methods while preserving overloads", () => {
    const { doc } = buildManifest();
    const signature = doc.kit.find((entry: any) => entry.name === "codesense.setUiState").signature;
    expect(signature).not.toContain("setUiState(args");
    expect(signature.match(/setUiState\(/g)).toHaveLength(2);
    expect(signature).toContain('topic: "ui:dock"');
    expect(signature).toContain("topic: string");
    expect(
      doc.invoke.find((entry: any) => entry.name === "setUiState").args.properties,
    ).toHaveProperty("patch");
  });
});

async function argumentForm(name: string) {
  const html = readFileSync(
    "codesense-docs/getting-started/content/primitives-explorer.html",
    "utf8",
  );
  const dom = new JSDOM(html, {
    url: `https://docs.test/primitives-explorer.html#invoke/${name}`,
    runScripts: "outside-only",
  });
  const invoke = vi.fn().mockResolvedValue({ ok: true });
  Object.assign(dom.window, {
    codesense: {
      invoke,
      docs: {
        hasEditableRepo: async () => false,
        mountInlineAffordance(slot: Element, { onRetry }: { onRetry: () => void }) {
          const button = dom.window.document.createElement("button");
          button.dataset.editableRepo = "true";
          button.textContent = "Use editable repo";
          button.addEventListener("click", onRetry);
          slot.appendChild(button);
        },
      },
    },
    fetch: vi.fn().mockResolvedValue({ ok: true, json: async () => buildManifest().doc }),
  });
  for (const script of dom.window.document.querySelectorAll("script:not([src])"))
    dom.window.eval(script.textContent!);
  dom.window.document.dispatchEvent(new dom.window.Event("DOMContentLoaded"));
  await vi.waitFor(() =>
    expect(
      dom.window.document.querySelector("#cs-pe-try-form, [data-editable-repo]"),
    ).not.toBeNull(),
  );
  dom.window.document.querySelector<HTMLButtonElement>("[data-editable-repo]")?.click();
  const form = dom.window.document.querySelector<HTMLFormElement>("#cs-pe-try-form")!;
  expect(form).not.toBeNull();
  return { dom, invoke, form };
}

describe("primitives explorer argument submission", () => {
  it.each([
    ["agent", { key: "saved-agent" }],
    ["routineRun", { runId: "saved-run" }],
    ["subscribe", { topic: "sessions", window: [0, 20] }],
    ["records.list", { collection: "notes", window: [0, 20] }],
    ["showMenu", { anchorRect: { x: 10, y: 20, width: 30, height: 40 }, row: { id: "one" } }],
    ["setStatus", { path: ["stories/one.md", "stories/two.md"], status: "review" }],
  ])("submits JSON inputs for %s through the real form", async (name, args) => {
    const { dom, invoke, form } = await argumentForm(name as string);
    try {
      const editor = form.querySelector<HTMLTextAreaElement>("textarea[data-json-args]");
      expect(editor, "this schema needs an editable JSON argument object").not.toBeNull();
      editor!.value = JSON.stringify(args);
      editor!.dispatchEvent(new dom.window.Event("input", { bubbles: true }));
      form.dispatchEvent(new dom.window.Event("submit", { bubbles: true, cancelable: true }));
      await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith(name, args));
    } finally {
      dom.window.close();
    }
  });
  it.each([
    ["inbox", { conversation: "agent-one", before: 10 }],
    ["gotoAgent", { agentId: "agent-one", preserveFocus: true }],
  ])("keeps primitive controls and submitted types for %s", async (name, args) => {
    const { dom, invoke, form } = await argumentForm(name as string);
    try {
      expect(form.querySelector("[data-json-args]")).toBeNull();
      for (const [key, value] of Object.entries(args)) {
        const input = form.querySelector<HTMLInputElement | HTMLSelectElement>(`[name="${key}"]`)!;
        expect(input).not.toBeNull();
        input.value = String(value);
        input.dispatchEvent(new dom.window.Event("input", { bubbles: true }));
      }
      form.dispatchEvent(new dom.window.Event("submit", { bubbles: true, cancelable: true }));
      await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith(name, args));
    } finally {
      dom.window.close();
    }
  });
});

describe("pane reload transport", () => {
  it("documents the shim-owned reload and focus messages without unknown payloads", () => {
    const { doc: manifest } = buildManifest() as { doc: Manifest };
    for (const name of [
      "before-reload",
      "before-reload-result",
      "focus-suppressed",
      "freeze",
      "presentation",
      "reload-state",
      "reload-warning",
      "restore-focus",
    ]) {
      const event = manifest.events.find((row) => row.name === `codesense:${name}`);
      expect(event, name).toBeDefined();
      expect(event!.payload, name).not.toContain("unknown");
      expect(event!.description, name).not.toBe("");
    }
  });
});
