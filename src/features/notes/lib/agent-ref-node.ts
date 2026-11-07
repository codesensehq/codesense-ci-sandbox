// SPDX-License-Identifier: MIT
import { Node, mergeAttributes } from "@tiptap/core";
import { AGENT_REF_SCHEME, agentHandleOfHref, agentNameOfLabel, formatAgentRef } from "./agent-ref";

/** The handoff link as an inline atom. Markdown keeps it as `[@Name](cs:agent:handle)`;
 *  on load those links become this node before the editor parses the HTML. */
export const AgentRef = Node.create({
  name: "agentRef",
  group: "inline",
  inline: true,
  atom: true,
  selectable: true,

  addAttributes() {
    return {
      handle: { default: "" },
      name: { default: "" },
    };
  },

  parseHTML() {
    return [
      {
        tag: "span[data-agent-ref]",
        getAttrs: (el) => ({
          handle: (el as HTMLElement).getAttribute("data-handle") ?? "",
          name: (el as HTMLElement).getAttribute("data-name") ?? "",
        }),
      },
    ];
  },

  renderHTML({ node, HTMLAttributes }) {
    return [
      "span",
      mergeAttributes(HTMLAttributes, {
        "data-agent-ref": "",
        "data-handle": node.attrs.handle,
        "data-name": node.attrs.name,
      }),
      `@${node.attrs.name}`,
    ];
  },

  renderText({ node }) {
    return `@${node.attrs.name as string}`;
  },

  addStorage() {
    return {
      markdown: {
        serialize(
          state: { write: (text: string) => void },
          node: { attrs: Record<string, unknown> },
        ) {
          state.write(
            formatAgentRef({ handle: String(node.attrs.handle), name: String(node.attrs.name) }),
          );
        },
        parse: {
          updateDOM(element: HTMLElement) {
            element.querySelectorAll(`a[href^="${AGENT_REF_SCHEME}"]`).forEach((a) => {
              const handle = agentHandleOfHref(a.getAttribute("href"));
              if (!handle) return;
              const span = a.ownerDocument.createElement("span");
              span.setAttribute("data-agent-ref", "");
              span.setAttribute("data-handle", handle);
              span.setAttribute("data-name", agentNameOfLabel(a.textContent ?? "") || handle);
              a.replaceWith(span);
            });
          },
        },
      },
    };
  },
});
