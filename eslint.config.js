// SPDX-License-Identifier: MIT
import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import prettier from "eslint-config-prettier";
import globals from "globals";
import { readFileSync } from "node:fs";
import { URL } from "node:url";

// SW-FU-C1: the first-use surfaces that are built from the shared button, empty
// state, spinner and inline alert. New ad-hoc variants on them fail lint.
const FIRST_USE_SURFACES = JSON.parse(
  readFileSync(new URL("./src/shared/components/first-use-surfaces.json", import.meta.url), "utf8"),
);
const RAW_FILL = "/(^|\\s)bg-(primary|foreground)(\\s|$)/";
const RAW_SPIN = "/(^|\\s)animate-spin(\\s|$)/";
const ERROR_NAME = "/^(e|err|error|cause)$/";
// First-use wording. "Context repository" is the explicit local create/attach
// contract; the remaining retired terms still fail lint.
const RETIRED_WORDS =
  "/\\b[Ww]orkspaces?\\b|Codesense|[Ss]ample epic|Invite a teammate|[Ss]ample pipeline/";
const VISIBLE_PROP = "/^(title|label|aria-label|placeholder|detail|body|why|message|description)$/";

export default tseslint.config(
  // Base JS recommended rules
  js.configs.recommended,

  // TypeScript recommended rules
  ...tseslint.configs.recommended,

  // Node-context globals + CommonJS module shape for `.cjs` scripts (e.g.
  // the SS-FU-5 MCP stub). The repo's root package.json declares
  // `"type": "module"` so plain `.js` files get ESM scope; CommonJS lives
  // in `.cjs`. Allow `require()` here since that's the CommonJS module form.
  {
    files: ["**/*.cjs"],
    languageOptions: {
      globals: {
        ...globals.node,
      },
      sourceType: "commonjs",
    },
    rules: {
      "@typescript-eslint/no-require-imports": "off",
      "no-redeclare": "off",
    },
  },

  // React Hooks rules
  {
    plugins: { "react-hooks": reactHooks },
    rules: {
      ...reactHooks.configs.recommended.rules,
      // setState in effects is intentional in several components (find-bar, quick-repo-dialog)
      "react-hooks/set-state-in-effect": "warn",
      // Ref access during render is intentional in drag-reorder pattern (viewer-top-nav)
      "react-hooks/refs": "warn",
    },
  },

  // Disable rules that conflict with Prettier
  prettier,

  // Project-specific overrides
  {
    rules: {
      // Allow unused vars prefixed with _
      "@typescript-eslint/no-unused-vars": [
        "error",
        {
          argsIgnorePattern: "^_",
          varsIgnorePattern: "^_",
          caughtErrorsIgnorePattern: "^_",
        },
      ],
      // Allow explicit any in test files and command wrappers
      "@typescript-eslint/no-explicit-any": "warn",
    },
  },

  // Shared chrome scripts inside the bundled docs context repo. These are
  // plain browser globals delivered to the viewer via `<script src>`, not
  // part of the app build, so lint them with the browser globals and relax
  // the TypeScript rules that assume module authoring. Empty-catch and
  // this-alias patterns are idiomatic in classic-script custom elements and
  // defensive bridge probes; disable both for this subtree.
  {
    files: ["codesense-docs/**/*.js"],
    languageOptions: {
      globals: {
        ...globals.browser,
      },
      sourceType: "script",
    },
    rules: {
      "@typescript-eslint/no-unused-vars": "off",
      "no-unused-vars": [
        "error",
        {
          argsIgnorePattern: "^_",
          varsIgnorePattern: "^_",
          caughtErrorsIgnorePattern: "^_",
        },
      ],
      "no-empty": ["error", { allowEmptyCatch: true }],
      "@typescript-eslint/no-this-alias": "off",
      "@typescript-eslint/no-unused-expressions": "off",
    },
  },

  // SW-FU-C1 (consistency spec, Rules 2, 4 and 5) on the first-use surfaces.
  {
    files: FIRST_USE_SURFACES,
    rules: {
      "no-restricted-imports": [
        "error",
        {
          paths: [
            {
              name: "lucide-react",
              importNames: ["Loader2"],
              message: "Wait with the shared Spinner / LoadingState (@/shared/components/spinner).",
            },
          ],
        },
      ],
      "no-restricted-syntax": [
        "error",
        {
          selector: `JSXAttribute[name.name='className'] Literal[value=${RAW_FILL}]`,
          message:
            "A raw bg-primary / bg-foreground fill is a hand-rolled primary button. Use AppButton (@/shared/components/app-button).",
        },
        {
          selector: `JSXAttribute[name.name='className'] TemplateElement[value.raw=${RAW_FILL}]`,
          message:
            "A raw bg-primary / bg-foreground fill is a hand-rolled primary button. Use AppButton (@/shared/components/app-button).",
        },
        {
          selector: `JSXAttribute[name.name='className'] Literal[value=${RAW_SPIN}]`,
          message: "Spin with the shared Spinner (motion-safe), not a raw animate-spin.",
        },
        {
          selector: `JSXAttribute[name.name='className'] TemplateElement[value.raw=${RAW_SPIN}]`,
          message: "Spin with the shared Spinner (motion-safe), not a raw animate-spin.",
        },
        {
          selector: `JSXExpressionContainer CallExpression[callee.name='String'][arguments.0.name=${ERROR_NAME}]`,
          message:
            "Don't render String(e). Show the failure with InlineAlert's `error` (userFacingErrorMessage).",
        },
        {
          selector: `JSXExpressionContainer MemberExpression[property.name='message'][object.name=${ERROR_NAME}]`,
          message:
            "Don't render e.message. Show the failure with InlineAlert's `error` (userFacingErrorMessage).",
        },
        // SW-FU-B8: a toast says what failed plus a safe reason (toastFailure), never
        // the raw thrown value.
        {
          selector: `CallExpression[callee.object.name='toast'] MemberExpression[property.name='message'][object.name=${ERROR_NAME}]`,
          message:
            "Don't toast e.message. Use toastFailure(title, e) (@/shared/lib/with-error-toast).",
        },
        {
          selector: `CallExpression[callee.name='toast'] MemberExpression[property.name='message'][object.name=${ERROR_NAME}]`,
          message:
            "Don't toast e.message. Use toastFailure(title, e) (@/shared/lib/with-error-toast).",
        },
        {
          selector: `CallExpression[callee.object.name='toast'] CallExpression[callee.name=/^(String|getErrorMessage)$/][arguments.0.name=${ERROR_NAME}]`,
          message:
            "Don't toast String(e) / getErrorMessage(e). Use toastFailure(title, e) (@/shared/lib/with-error-toast).",
        },
        // SW-FU-C5: first-use copy speaks the capstone's words.
        {
          selector: `JSXText[value=${RETIRED_WORDS}]`,
          message:
            "Retired word on a first-use screen (consistency spec, Rule 6): say team, CodeSense, sample project.",
        },
        {
          selector: `JSXAttribute[name.name=${VISIBLE_PROP}] Literal[value=${RETIRED_WORDS}]`,
          message:
            "Retired word on a first-use screen (consistency spec, Rule 6): say team, CodeSense, sample project.",
        },
        {
          selector: `JSXAttribute[name.name=${VISIBLE_PROP}] TemplateElement[value.raw=${RETIRED_WORDS}]`,
          message:
            "Retired word on a first-use screen (consistency spec, Rule 6): say team, CodeSense, sample project.",
        },
        {
          selector: `Property[key.name=${VISIBLE_PROP}] Literal[value=${RETIRED_WORDS}]`,
          message:
            "Retired word on a first-use screen (consistency spec, Rule 6): say team, CodeSense, sample project.",
        },
        {
          selector: "JSXText[value=/\\.\\.\\./]",
          message: "Write an ellipsis (…), never three dots.",
        },
        {
          selector: "JSXAttribute > Literal[value=/\\.\\.\\./]",
          message: "Write an ellipsis (…), never three dots.",
        },
      ],
    },
  },

  // Test file relaxations
  {
    files: ["**/*.test.*", "**/*.spec.*", "tests/**/*"],
    rules: {
      "@typescript-eslint/no-explicit-any": "off",
    },
  },

  // Ignore patterns
  {
    ignores: [
      "dist/",
      "node_modules/",
      "src-tauri/",
      "target/",
      "target-*",
      "target-*/**",
      "coverage/",
      "test-results/",
      "playwright-report/",
      ".playwright-mcp/",
      "_bmad/",
      "_bmad-output/",
      // Vendored local tooling — Claude skills/hooks/settings (mostly
      // gitignored), not app source. Skill templates (e.g. WDS prototype
      // snippets) ship their own runtime globals and shouldn't be linted here.
      ".claude/",
      ".agents/",
      "scripts/",
      // The project website: plain browser JS outside the app.
      "site/",
      // Story proof artifacts (probes, clients, captured logs) are not app source.
      "design/**",
      "**/__snapshots__/**",
    ],
  },

  // Generated ambient declarations use `declare var` for globals.
  {
    files: ["codesense-docs/**/generated/*.d.ts"],
    rules: { "no-var": "off" },
  },
);
