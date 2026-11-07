// SPDX-License-Identifier: MIT
import { commitTextEditOnBlur } from "@/shared/lib/text-menu-focus";
import { useState, useRef, useEffect } from "react";
import { NodeViewWrapper, NodeViewContent } from "@tiptap/react";
import { Send } from "lucide-react";
import type { NodeViewProps } from "@tiptap/react";

import { InfoTip } from "@/components/ui/tooltip";

import { COMMON_LANGUAGES } from "@/features/notes/editor/code-block-languages";

interface CodeBlockNodeViewProps extends NodeViewProps {
  /** Opens "Send to…" for the block's text, anchored on the block. */
  onSend?: (text: string, anchor: HTMLElement) => void;
}

export function CodeBlockNodeView({ node, updateAttributes, onSend }: CodeBlockNodeViewProps) {
  const language = node.attrs.language || "";
  const isPlaintext = !language || language === "plaintext";
  const [editing, setEditing] = useState(false);
  const [editValue, setEditValue] = useState(language);
  const inputRef = useRef<HTMLInputElement>(null);
  const [suggestions, setSuggestions] = useState<string[]>([]);

  const [dropdownPos, setDropdownPos] = useState<{ top: number; left: number } | null>(null);

  useEffect(() => {
    if (editing && inputRef.current) {
      inputRef.current.focus();
      inputRef.current.select();
      const rect = inputRef.current.getBoundingClientRect();
      setDropdownPos({ top: rect.bottom + 2, left: rect.left });
    }
  }, [editing]);

  const handleLanguageClick = () => {
    setEditValue(isPlaintext ? "" : language);
    setSuggestions(COMMON_LANGUAGES);
    setEditing(true);
  };

  const commitLanguage = (lang: string) => {
    const trimmed = lang.trim().toLowerCase();
    updateAttributes({ language: trimmed || null });
    setEditing(false);
  };

  const handleInputChange = (val: string) => {
    setEditValue(val);
    if (val) {
      setSuggestions(COMMON_LANGUAGES.filter((l) => l.startsWith(val.toLowerCase())));
    } else {
      setSuggestions(COMMON_LANGUAGES);
    }
  };

  const handleSend = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    const text = node.textContent;
    if (text && onSend) onSend(text, e.currentTarget as HTMLElement);
  };

  return (
    <NodeViewWrapper className="scratch-code-block-wrapper">
      <div className="scratch-code-block-header">
        {/* Language label / picker */}
        <div className="scratch-code-block-lang">
          {editing ? (
            <div className="relative">
              <input
                ref={inputRef}
                type="text"
                value={editValue}
                onChange={(e) => handleInputChange(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    commitLanguage(editValue);
                  } else if (e.key === "Escape") {
                    setEditing(false);
                  }
                  e.stopPropagation();
                }}
                onBlur={(event) => {
                  // Delay to allow click on suggestion
                  commitTextEditOnBlur(event.currentTarget, () => {
                    setTimeout(() => setEditing(false), 150);
                  });
                }}
                className="scratch-code-block-lang-input"
                placeholder="language..."
              />
              {suggestions.length > 0 && dropdownPos && (
                <div
                  className="scratch-code-block-lang-dropdown"
                  style={{ top: dropdownPos.top, left: dropdownPos.left }}
                >
                  {suggestions.slice(0, 8).map((lang) => (
                    <button
                      key={lang}
                      className="scratch-code-block-lang-option"
                      onMouseDown={(e) => {
                        e.preventDefault();
                        commitLanguage(lang === "plaintext" ? "" : lang);
                      }}
                    >
                      {lang}
                    </button>
                  ))}
                </div>
              )}
            </div>
          ) : (
            <button
              className="scratch-code-block-lang-label"
              onClick={handleLanguageClick}
              contentEditable={false}
            >
              {isPlaintext ? "prompt" : language}
            </button>
          )}
        </div>

        <InfoTip label="Send to an agent or the terminal">
          <button
            type="button"
            className="scratch-code-block-send"
            onMouseDown={(e) => e.preventDefault()}
            onClick={handleSend}
            contentEditable={false}
          >
            <Send size={12} />
            <span>Send</span>
            <kbd className="nt-kbd">⌘↵</kbd>
          </button>
        </InfoTip>
      </div>

      <NodeViewContent as="div" className="scratch-code-block-content" />
    </NodeViewWrapper>
  );
}
