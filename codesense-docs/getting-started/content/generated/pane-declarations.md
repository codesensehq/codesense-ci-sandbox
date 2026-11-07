# Pane declarations

Generated from panes/mod.rs and manifest.rs. Manifest declarations are flattened into each panes entry. Missing declaration fields use their serde defaults. Commands require id, label and scope. HTML and YAML declarations must agree when both are present.

[JSON Schema](pane-declaration.schema.json) · [SDK typings](codesense.d.ts) · [Capabilities](capabilities.md) · [Slots](slots.md)

| Field | HTML meta tag (content) | YAML entry | Default |
| --- | --- | --- | --- |
| badge | codesense:badge (string) | badge | null |
| slots | codesense:slots (comma or whitespace separated) | slots | [] |
| surface | codesense:surface (string) | surface | null |
| capabilities | codesense:capabilities (comma or whitespace separated) | capabilities | [] |
| commands | codesense:commands (JSON array) | commands | [] |

## YAML manifest entry

| Field | Type | Required | Default |
| --- | --- | --- | --- |
| badge | [{"type":"string"},{"type":"null"}] | false | "—" |
| slots | array | false | [] |
| surface | [{"type":"string"},{"type":"null"}] | false | "—" |
| capabilities | array | false | [] |
| commands | array | false | [] |
| id | string | true | "—" |
| title | [{"type":"string"},{"type":"null"}] | false | "—" |
| html | string | true | "—" |
| icon | [{"type":"string"},{"type":"null"}] | false | "—" |
| showsOn | array | false | [] |
| priority | [{"type":"integer","minimum":0},{"type":"null"}] | false | "—" |
| collapsedByDefault | boolean | false | false |
| reactsTo | array | false | [] |

## Commands (JSON in meta; objects in YAML)

| Field | Type | Required |
| --- | --- | --- |
| id | string | true |
| label | string | true |
| icon | [{"type":"string"},{"type":"null"}] | false |
| defaultChord | [{"type":"string"},{"type":"null"}] | false |
| scope | global, slot-visible, slot-focused | true |
| humanOnly | boolean | false |

```html
<meta name="codesense:slots" content="right-pane">
<meta name="codesense:capabilities" content="sessions:read">
<meta name="codesense:surface" content="notes">
<meta name="codesense:commands" content='[ {"id":"refresh","label":"Refresh","scope":"slot-visible"} ]'>
```

```yaml
version: 1
panes:
  - id: example
    html: example.html
    slots: [right-pane]
    capabilities: [sessions:read]
    surface: notes
    commands:
      - id: refresh
        label: Refresh
        scope: slot-visible
```
