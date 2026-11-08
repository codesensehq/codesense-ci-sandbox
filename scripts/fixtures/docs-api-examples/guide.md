# Guide

Answer with `reply {message_id, text}`, never `reply {messageId}`.
Or call `reply` {body} as prose.

```js
cs.act('worktrees', {})
cs.act('listOrgsTeams', {})
```

A stray ` backtick here

prose: reply { body } is not code, but `reply {text, to}` is.

<!-- `reply {body}` is commented out -->

Write `<!--` to open an HTML comment.

Then `reply {body}` is still code.

Close it with `-->`.

```html
<!-- a comment opened in a fenced example
```
`reply {kind}` is prose code after the fence -->

<!--
```js
reply { body }
```
-->

<div data-api-lint="off">

```js
reply { body }   // deliberately wrong
```

</div>

Mark it with `<div data-api-lint="off">` around the example.

`reply {kind}` after that sentence is still checked.

Close it with `</div>`.
