// SPDX-License-Identifier: MIT
/**
 * SLW-DV-8 (AC-2) — resolve the enclosing scope of a document position for the
 * sticky diff breadcrumb.
 *
 * Works off the Lezer parse tree (the same grammars the editor already loads for
 * highlighting), so it degrades exactly with parser coverage:
 *   - real nested grammars (md, ts/js/jsx/tsx, json, yaml, python) → a scope chain;
 *   - flat `StreamLanguage` grammars (rust, shell, xml) and `plain` → `null`
 *     (the caller hides the bar rather than inventing scope we can't derive).
 *
 * Kept free of any `@codemirror/*` import so it is unit-testable against a minimal
 * node shape — but it is exercised against the REAL grammars in the tests.
 */

import type { LanguageId } from "./cm-loader";

/** The subset of a Lezer `SyntaxNode` this resolver reads. */
export interface ScopeNode {
  readonly name: string;
  readonly from: number;
  readonly to: number;
  readonly parent: ScopeNode | null;
  readonly firstChild: ScopeNode | null;
  readonly nextSibling: ScopeNode | null;
}

/** The subset of a Lezer `Tree` this resolver reads. */
export interface ScopeTree {
  readonly topNode: ScopeNode;
  resolveInner(pos: number, side: number): ScopeNode;
}

export type BreadcrumbMode = "code" | "markdown" | "none";

/** Which resolution strategy a language supports. Anything without a real nested
 *  parse tree resolves to `"none"`. */
export function breadcrumbModeFor(langId: LanguageId): BreadcrumbMode {
  switch (langId) {
    case "markdown":
      return "markdown";
    case "typescript":
    case "javascript":
    case "python":
    case "json":
    case "yaml":
      return "code";
    default:
      // Flat StreamLanguage grammars (and css/html, whose trees carry no named
      // code scopes this resolver recognises) have nothing to show.
      return "none";
  }
}

// A scope-defining node: named code containers (capture 1 = the display keyword)
// plus structured-data keys (capture 2 = Property/Pair, shown as the bare key).
// Anchored so `VariableDeclaration` (noise) does not match while
// `FunctionDeclaration` does. This regex is the single source of truth for both
// "is this a scope?" and "what keyword precedes its name?".
const SCOPE_NODE_RE =
  /^(Function|Class|Method|Interface|Enum|Namespace|Module|Struct)(?:Declaration|Definition|Expression|Signature)$|^(Property|Pair)$/;

// Child nodes that carry the scope's identifier/key text.
const NAME_NODE_RE = /^(VariableDefinition|PropertyDefinition|VariableName|PropertyName|Property)$/;

const MAX_LABEL = 60;

/** The display keyword for a scope node ("class", "function", …), `""` for a
 *  structured-data key (bare name), or `null` when the node is not a scope. */
function scopeKeyword(nodeName: string): string | null {
  const m = SCOPE_NODE_RE.exec(nodeName);
  if (!m) return null;
  return m[1] ? m[1].toLowerCase() : "";
}

function truncate(s: string): string {
  const t = s.trim();
  return t.length > MAX_LABEL ? `${t.slice(0, MAX_LABEL - 1)}…` : t;
}

/** The identifier/key text of a scope node prefixed by `keyword`, or `null` for
 *  an anonymous one. */
function scopeLabel(
  node: ScopeNode,
  sliceDoc: (from: number, to: number) => string,
  keyword: string,
): string | null {
  let child = node.firstChild;
  while (child) {
    if (NAME_NODE_RE.test(child.name)) {
      const raw = sliceDoc(child.from, child.to).replace(/^["']|["']$/g, "");
      if (!raw.trim()) return null;
      return truncate(keyword ? `${keyword} ${raw}` : raw);
    }
    child = child.nextSibling;
  }
  return null;
}

/** Ancestor scope chain (outermost → innermost) at `pos`, or `null` if none. */
function codeScopeChain(
  tree: ScopeTree,
  pos: number,
  sliceDoc: (from: number, to: number) => string,
): string[] | null {
  let node: ScopeNode | null = tree.resolveInner(pos, -1);
  const chain: string[] = [];
  while (node) {
    const keyword = scopeKeyword(node.name);
    if (keyword !== null) {
      const label = scopeLabel(node, sliceDoc, keyword);
      if (label) chain.unshift(label);
    }
    node = node.parent;
  }
  return chain.length ? chain : null;
}

/** The nearest markdown heading at or before `pos`, or `null`. */
// The last (document-order) heading node starting at or before `pos`, searched
// at any depth — headings can be nested inside a blockquote or list item, not
// only at the document top level. Children are ordered, so a subtree that starts
// after `pos` (and everything after it) can be pruned.
function findNearestHeading(node: ScopeNode, pos: number): ScopeNode | null {
  let best: ScopeNode | null = null;
  let child = node.firstChild;
  while (child && child.from <= pos) {
    if (/Heading/.test(child.name)) best = child;
    const nested = findNearestHeading(child, pos);
    if (nested) best = nested;
    child = child.nextSibling;
  }
  return best;
}

function nearestHeading(
  tree: ScopeTree,
  pos: number,
  sliceDoc: (from: number, to: number) => string,
): string[] | null {
  const best = findNearestHeading(tree.topNode, pos);
  if (!best) return null;
  // Take the first line (a Setext heading's node spans its `===`/`---` underline
  // too) and strip ATX markers — leading `###` and an optional trailing `###`.
  const firstLine = sliceDoc(best.from, best.to).split("\n", 1)[0] ?? "";
  const text = firstLine
    .replace(/^#+\s*/, "")
    .replace(/\s+#+\s*$/, "")
    .trim();
  return text ? [truncate(text)] : null;
}

/**
 * Resolve the scope chain for `pos`. Returns the outermost → innermost labels, or
 * `null` when the language has no derivable scope or the position sits at top level.
 */
export function resolveScopeChain(
  tree: ScopeTree,
  pos: number,
  sliceDoc: (from: number, to: number) => string,
  mode: BreadcrumbMode,
): string[] | null {
  if (mode === "none") return null;
  if (mode === "markdown") return nearestHeading(tree, pos, sliceDoc);
  return codeScopeChain(tree, pos, sliceDoc);
}
