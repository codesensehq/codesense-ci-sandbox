// SPDX-License-Identifier: MIT
import type { NodeViewRenderer } from "@tiptap/core";
import type { StatusMarkKind } from "@/shared/lib/status-mark";
import { statusMarkDom } from "./status-mark-dom";

import { COMMON_LANGUAGES } from "./code-block-languages";

export interface AgentLook {
  name: string;
  status?: { mark: StatusMarkKind; word: string };
  avatar?: string;
}
export interface DomViewOptions {
  agents?: { lookup(handle: string): AgentLook | null | undefined };
  onAgentClick?(handle: string): void;
  onCodeSend?(text: string, rect: DOMRect): void;
}
function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  cls: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  node.className = cls;
  if (text) node.textContent = text;
  return node;
}
export function domNodeViews(options: DomViewOptions) {
  const refreshers = new Set<() => void>();
  const agentRef: NodeViewRenderer = ({ node: initial }) => {
    let node = initial;
    const dom = el("span", "nt-agent-ref");
    dom.contentEditable = "false";
    const render = () => {
      const handle = String(node.attrs.handle);
      const look = options.agents?.lookup(handle);
      const name = look?.name || String(node.attrs.name);
      const status = look?.status ?? { mark: "ended", word: "not running" };
      const chip = el("button", "nt-agent-chip");
      chip.type = "button";
      chip.dataset.agentHandle = handle;
      chip.dataset.mark = status.mark;
      chip.title = look ? `Show ${name}` : `${name} is not running`;
      chip.append(statusMarkDom(status.mark));
      if (look?.avatar) {
        const face = el("img", "nt-agent-chip-face");
        face.src = look.avatar;
        face.alt = "";
        face.width = face.height = 14;
        chip.append(face);
      }
      chip.append(
        el("span", "nt-agent-chip-name", name),
        el("span", "nt-agent-chip-status", status.word.toLowerCase()),
      );
      chip.onmousedown = (e) => e.preventDefault();
      chip.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        options.onAgentClick?.(handle);
      };
      dom.replaceChildren(chip);
    };
    refreshers.add(render);
    render();
    return {
      dom,
      stopEvent: () => true,
      ignoreMutation: () => true,
      update(next) {
        if (next.type !== node.type) return false;
        node = next;
        render();
        return true;
      },
      destroy() {
        refreshers.delete(render);
      },
    };
  };
  const codeBlock: NodeViewRenderer = ({ node: initial, editor, getPos }) => {
    let node = initial;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const dom = el("div", "scratch-code-block-wrapper");
    const header = el("div", "scratch-code-block-header");
    header.contentEditable = "false";
    const lang = el("div", "scratch-code-block-lang");
    const label = el("button", "scratch-code-block-lang-label");
    label.type = "button";
    const send = el("button", "scratch-code-block-send");
    send.type = "button";
    send.title = "Send to an agent";
    send.innerHTML =
      '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14.536 21.686a.5.5 0 0 0 .937-.024l6.5-19a.496.496 0 0 0-.635-.635l-19 6.5a.5.5 0 0 0-.024.937l7.93 3.18a2 2 0 0 1 1.112 1.11z"/><path d="m21.854 2.147-10.94 10.939"/></svg><span>Send</span><kbd class="nt-kbd">⌘↵</kbd>';
    send.onmousedown = (e) => e.preventDefault();
    send.onclick = (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (node.textContent) options.onCodeSend?.(node.textContent, send.getBoundingClientRect());
    };
    const contentDOM = el("div", "scratch-code-block-content");
    header.append(lang, send);
    dom.append(header, contentDOM);
    const reset = () => {
      clearTimeout(timer);
      label.textContent =
        !node.attrs.language || node.attrs.language === "plaintext"
          ? "prompt"
          : node.attrs.language;
      lang.replaceChildren(label);
    };
    const commit = (value: string) => {
      const pos = getPos();
      const language = value.trim().toLowerCase();
      if (typeof pos === "number")
        editor.view.dispatch(
          editor.state.tr.setNodeMarkup(pos, undefined, {
            ...node.attrs,
            language: language === "plaintext" ? null : language || null,
          }),
        );
      reset();
      editor.commands.focus();
    };
    label.onclick = () => {
      if (!editor.isEditable) return;
      const input = el("input", "scratch-code-block-lang-input");
      input.placeholder = "language...";
      input.value = node.attrs.language === "plaintext" ? "" : node.attrs.language || "";
      const dropdown = el("div", "scratch-code-block-lang-dropdown");
      const suggest = () => {
        dropdown.replaceChildren(
          ...COMMON_LANGUAGES.filter((l) => l.startsWith(input.value.toLowerCase()))
            .slice(0, 8)
            .map((l) => {
              const option = el("button", "scratch-code-block-lang-option", l);
              option.type = "button";
              option.onmousedown = (e) => {
                e.preventDefault();
                commit(l);
              };
              return option;
            }),
        );
      };
      input.oninput = suggest;
      input.onkeydown = (e) => {
        if (e.key === "Enter" || e.key === "Escape") {
          e.preventDefault();
          if (e.key === "Enter") commit(input.value);
          else {
            reset();
            editor.commands.focus();
          }
        }
        e.stopPropagation();
      };
      input.onfocus = () => clearTimeout(timer);
      input.onblur = () => {
        clearTimeout(timer);
        timer = setTimeout(() => {
          // Architecture: only the shim's active text menu defers this blur dismissal.
          const frame = input.ownerDocument.defaultView as
            | (Window & { codesense?: { readonly textMenuOpen: boolean } })
            | null;
          if (!frame?.codesense?.textMenuOpen) reset();
        }, 150);
      };
      lang.replaceChildren(input, dropdown);
      suggest();
      input.focus();
      input.select();
      const rect = input.getBoundingClientRect();
      dropdown.style.top = `${rect.bottom + 2}px`;
      dropdown.style.left = `${rect.left}px`;
    };
    reset();
    return {
      dom,
      contentDOM,
      stopEvent: (event) => header.contains(event.target as Node),
      ignoreMutation: (mutation) =>
        mutation.type !== "selection" && header.contains(mutation.target),
      update(next) {
        if (next.type !== node.type) return false;
        node = next;
        reset();
        return true;
      },
      destroy() {
        clearTimeout(timer);
      },
    };
  };
  return { agentRef, codeBlock, refreshAgents: () => refreshers.forEach((fn) => fn()) };
}
