# Themes

Settings → Appearance has one theme for the app, documents, editors and panes. Mode chooses light, dark or the system preference. Each theme supplies both modes. Terminal colours defaults to **Match app theme**; an explicit terminal palette is a person-wide override. Agents can change `settings.appTheme` through `set_viewer_state`; the default is `codesense`.

## Theme files

Bundled themes live at `src/themes/<id>.tokens.json`. A theme has an `id`, a plain-text `name`, and required `light` and `dark` groups. The format is the DTCG 2025.10-shaped subset described by `src/themes/theme.schema.json`. Display names have control and bidi characters stripped. This version ships bundled themes; loading user or team theme files is future work.

Roles describe purpose: `surface.{canvas,sidebar,viewer,terminal,overlay}.{bg,hover,selected}`, text `primary` and `muted` per surface, `border.default` (dividers), `border.control` (controls), `focus.ring`, `accent`, `severity`, `git`, `code` and `terminal` (including 16 ANSI colours). The viewer also has `raised` and `inset` surfaces. Compatibility roles preserve existing native variable names. Surface boundaries reset text and severity, so a light app-painted child does not inherit ink intended for a dark terminal.

Every leaf has `$type` and `$value`. Colours are typed sRGB objects with three components in 0–1 and optional alpha in 0–1. Dimensions have a finite `value` and an allowed unit (`px`, `rem`, `em`, `%`, `ms`); number tokens are finite. Font names are allow-listed (`Geist Variable`, `ui-monospace`, `sans-serif`, `monospace`). CSS strings, URLs and `$extensions` are refused. Files are limited to 256 KB and 2,048 tokens. An alias such as `{light.accent.default}` must reference a concrete token of the same type: one alias level, with no chains.

```json
{
  "$type": "color",
  "$value": { "colorSpace": "srgb", "components": [0.035, 0.412, 0.855] }
}
```

[The complete example](examples/themes/example.tokens.json) includes both modes and the full role set; copy it as `themes/<id>.tokens.json` when authoring a bundled theme. `compileTheme(theme, mode)` validates typed values and emits CSS properties. `pnpm gen:theme` regenerates app defaults and `/kit/v1/tokens.css`; `pnpm gen:theme:check` detects stale output. Theme files change only through a person or agent's explicit file edits. Themes do not sync, pull or push anything.

## Optional review hue

`{light,dark}.review.status` is an optional colour token that gives Review and PR status their own hue without touching syntax colours. It sets the kit aliases `--cs-story-review`, `--cs-phase-review` and `--cs-phase-pr`. When absent, those aliases fall back to `code.function`. Add it when a theme's function-syntax colour matches its success colour, so in-review work would otherwise look like done. Both modes are required whenever the token is present. Dracula uses it (dark `#bd93f9`, light `#644ac9`):

```json
{
  "light": {
    "review": {
      "status": {
        "$type": "color",
        "$value": { "colorSpace": "srgb", "components": [0.392, 0.29, 0.788] }
      }
    }
  },
  "dark": {
    "review": {
      "status": {
        "$type": "color",
        "$value": { "colorSpace": "srgb", "components": [0.741, 0.576, 0.976] }
      }
    }
  }
}
```

## Contrast gate

The rendered cascade is checked for every bundled theme, mode and surface, including hover and selection. Text, code, git letters and severity text require 4.5:1. Control borders, solid focus rings and icons require 3:1; ANSI colours 1–15 require 3:1 against the terminal background. Muted text must differ from primary by at least 1.2:1. Alpha is composited in sRGB before measurement. Severity colour distinctness and ANSI over terminal selection are reported without failing the gate. Icons and text labels still communicate severity and selection. The exported `CONTRAST_THRESHOLDS` constant owns the thresholds.

## Panes follow the host

Use surface-free kit tokens, for example `--cs-bg`, `--cs-fg`, `--cs-muted-fg`, `--cs-surface`, `--cs-border`, `--cs-accent`, `--cs-accent-fg`, `--cs-link`, `--cs-danger`, `--cs-success` and `--cs-warning`. The host resolves them at the frame's nearest `data-surface` boundary. The existing token names keep their meanings; `--cs-border-control` and `--cs-focus-ring` are additive. Story lifecycle colours use `--cs-story-planning`, `--cs-story-todo`, `--cs-story-in-progress`, `--cs-story-blocked`, `--cs-story-review`, `--cs-story-qa` and `--cs-story-done`. Agent states use `--cs-state-*`; progress phases use `--cs-phase-*`.

```js
const stop = codesense.onTheme((theme) => {
  // CSS-based UI already repaints; use this for canvas or other imperative drawing.
  redraw(theme.tokens["--cs-fg"], theme.tokens["--cs-bg"]);
});
window.addEventListener("pagehide", stop, { once: true });
```

One global LiveHub `theme` topic carries `{ themeId, mode, rev }`. Visible panes receive `codesense:theme` immediately. Hidden panes are marked stale and resolve the latest tokens on show, including dock and reparented panes. Do not add a MutationObserver, polling timer or reload to follow theme changes.
