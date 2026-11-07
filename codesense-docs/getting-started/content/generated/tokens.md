# Dashboard tokens

87 kit and host tokens. Bundled CodeSense light/dark; selected app themes and host overrides take precedence. Expressions remain CSS expressions where the runtime resolves them. The fallback stylesheet uses this same model. Live pages should use the host theme, not prefers-color-scheme.

| Token | Light | Dark | Host source / default |
| --- | --- | --- | --- |
| --cs-space-1 | 4px | 4px | tokens.css |
| --cs-space-2 | 8px | 8px | tokens.css |
| --cs-space-3 | 12px | 12px | tokens.css |
| --cs-space-4 | 16px | 16px | tokens.css |
| --cs-space-5 | 24px | 24px | tokens.css |
| --cs-space-6 | 32px | 32px | tokens.css |
| --cs-radius-sm | 4px | 4px | tokens.css |
| --cs-radius | 6px | 6px | tokens.css |
| --cs-radius-lg | 10px | 10px | tokens.css |
| --cs-radius-full | 999px | 999px | tokens.css |
| --cs-text-xs | 11px | 11px | tokens.css |
| --cs-text-sm | 12px | 12px | tokens.css |
| --cs-text-base | 13px | 13px | tokens.css |
| --cs-text-lg | 15px | 15px | tokens.css |
| --cs-text-xl | 18px | 18px | tokens.css |
| --cs-leading | 1.5 | 1.5 | tokens.css |
| --cs-font-mono | ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace | ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace | tokens.css |
| --cs-hover | color-mix(in srgb, var(--cs-fg, CanvasText) 6%, transparent) | color-mix(in srgb, var(--cs-fg, CanvasText) 6%, transparent) | tokens.css |
| --cs-inset | color-mix(in srgb, var(--cs-fg, CanvasText) 4%, var(--cs-bg, Canvas)) | color-mix(in srgb, var(--cs-fg, CanvasText) 4%, var(--cs-bg, Canvas)) | tokens.css |
| --cs-shadow-lg | 0 0 0 1px rgb(0 0 0 / 0.04), 0 12px 32px rgb(0 0 0 / 0.28), 0 2px 8px rgb(0 0 0 / 0.16) | 0 0 0 1px rgb(0 0 0 / 0.04), 0 12px 32px rgb(0 0 0 / 0.28), 0 2px 8px rgb(0 0 0 / 0.16) | tokens.css |
| --cs-shadow | 0 1px 2px rgb(0 0 0 / 0.12), 0 2px 8px rgb(0 0 0 / 0.06) | 0 1px 2px rgb(0 0 0 / 0.12), 0 2px 8px rgb(0 0 0 / 0.06) | tokens.css |
| --cs-tree-accent-1 | hsl(340, 55%, 45%) | hsl(340, 55%, 65%) | tokens.css |
| --cs-tree-accent-2 | hsl(285, 45%, 45%) | hsl(285, 45%, 65%) | tokens.css |
| --cs-bg | #ffffff | #0d1117 | --cs-viewer-bg → --background |
| --cs-fg | #1f2328 | #e6edf3 | color → --foreground |
| --cs-muted-fg | #636c76 | #8b949e | --cs-sidebar-text-muted → --muted-foreground |
| --cs-border | #d0d7de | #30363d | --cs-sidebar-border → --border |
| --cs-border-control | #818b98 | #6e7681 | tokens.css |
| --cs-focus-ring | #0969da | #58a6ff | tokens.css |
| --cs-surface | #f7f8fa | #262b32 | --cs-workspace-card-bg → --card |
| --cs-accent | #0969da | #58a6ff | --cs-viewer-accent → --primary |
| --cs-accent-fg | #ffffff | #0d1117 | --cs-viewer-bg → --primary-foreground |
| --cs-highlight | #0969da | #58a6ff | --accent |
| --cs-link | #0969da | #58a6ff | --tw-prose-links → --primary |
| --cs-danger | #cd1f2c | #f85149 | --destructive |
| --cs-success | #0f7931 | #3fb950 | --cs-status-added |
| --cs-warning | #973c00 | #ffb900 | --cs-status-modified |
| --cs-deleted | #cf222e | #f85149 | --cs-status-deleted |
| --cs-selected | #eaeef2 | #1c2128 | tokens.css |
| --cs-git-added | #0f7931 | #3fb950 | tokens.css |
| --cs-git-modified | #926000 | #d29922 | tokens.css |
| --cs-git-deleted | #cf222e | #f85149 | tokens.css |
| --cs-git-conflict | #8250df | #d2a8ff | tokens.css |
| --cs-sidebar-selected | #eaeef2 | #1c2128 | tokens.css |
| --cs-sidebar-drop-ring | #0969da | #58a6ff | tokens.css |
| --cs-sidebar-bg | #f6f8fa | #161b22 | --cs-sidebar-bg |
| --cs-sidebar-fg | #1f2328 | #e6edf3 | --cs-sidebar-text |
| --cs-sidebar-text | #1f2328 | #e6edf3 | tokens.css |
| --cs-sidebar-selected-text | #1f2328 | #e6edf3 | tokens.css |
| --cs-status-needs | #b36b10 | #fcaa2b | tokens.css |
| --cs-status-busy | #216d84 | #4aebeb | tokens.css |
| --cs-status-failed | #af2938 | #ff8378 | tokens.css |
| --cs-status-idle | #7d8287 | #aab0b7 | tokens.css |
| --cs-code-keyword | #cf222e | #ff7b72 | --cs-code-keyword |
| --cs-code-string | #0a3069 | #a5d6ff | --cs-code-string |
| --cs-code-comment | #68717b | #8b949e | --cs-code-comment |
| --cs-code-number | #0550ae | #79c0ff | tokens.css |
| --cs-code-constant | #0550ae | #79c0ff | tokens.css |
| --cs-code-function | #8250df | #d2a8ff | tokens.css |
| --cs-code-variable | #953800 | #ffa657 | tokens.css |
| --cs-code-type | #953800 | #ffa657 | tokens.css |
| --cs-code-operator | #0550ae | #79c0ff | tokens.css |
| --cs-code-punctuation | #24292f | #e6edf3 | tokens.css |
| --cs-code-meta | #116329 | #7ee787 | tokens.css |
| --cs-code-regexp | #116329 | #7ee787 | tokens.css |
| --cs-story-planning | #636c76 | #8b949e | tokens.css |
| --cs-story-todo | #636c76 | #8b949e | tokens.css |
| --cs-story-in-progress | #0063d3 | #58a6ff | tokens.css |
| --cs-story-blocked | #cd1f2c | #f85149 | tokens.css |
| --cs-story-review | #8250df | #d2a8ff | tokens.css |
| --cs-story-qa | #973c00 | #ffb900 | tokens.css |
| --cs-story-done | #0f7931 | #3fb950 | tokens.css |
| --cs-state-failed | #af2938 | #ff8378 | tokens.css |
| --cs-state-running | #216d84 | #4aebeb | tokens.css |
| --cs-state-idle | #7d8287 | #aab0b7 | tokens.css |
| --cs-state-needs-you | #b36b10 | #fcaa2b | tokens.css |
| --cs-state-exited | #7d8287 | #aab0b7 | tokens.css |
| --cs-state-offline | #7d8287 | #aab0b7 | tokens.css |
| --cs-state-unknown | #7d8287 | #aab0b7 | tokens.css |
| --cs-phase-planning | #636c76 | #8b949e | tokens.css |
| --cs-phase-dev | #0063d3 | #58a6ff | tokens.css |
| --cs-phase-review | #8250df | #d2a8ff | tokens.css |
| --cs-phase-pr | #8250df | #d2a8ff | tokens.css |
| --cs-phase-blocked | #cd1f2c | #f85149 | tokens.css |
| --cs-phase-done | #0f7931 | #3fb950 | tokens.css |
| --cs-font-sans | "Geist Variable", sans-serif | "Geist Variable", sans-serif | font-family |
| --cs-color-scheme | light | dark | computed from surface background luminance |
