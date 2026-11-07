// SPDX-License-Identifier: MIT
import { fnBody, matchBracket, parseItems, stringLiterals } from "./rust.mjs";

/** Read the conditional grant branches Rust executes before falling back to spec.grant. */
export function conditionalVerbGrants(app, verbs) {
  const source = fnBody(app.file("src-tauri/src/verbs/registry.rs"), "required_capability");
  const binding = fnBody(app.file("src-tauri/src/records/binding.rs"), "capability");
  const scopes = parseItems(app.file("src-tauri/src/records/schema.rs")).get("Scope").members;
  const privateVariant = binding.match(/scope\s*==\s*Scope::(\w+)/)?.[1];
  const boundGrants = new Map(
    [...binding.matchAll(/\(\s*(true|false)\s*,\s*(true|false)\s*\)\s*=>\s*"([^"]+)"/g)].map(
      (match) => [`${match[1]}:${match[2]}`, match[3]],
    ),
  );
  if (!privateVariant || boundGrants.size !== 4)
    throw new Error("Unsupported records capability policy");
  const grants = Object.fromEntries(verbs.map((verb) => [verb.name, []]));
  let offset = 0;
  while (offset < source.length) {
    const next = /\bif\s+/.exec(source.slice(offset));
    if (!next) break;
    const start = offset + next.index + next[0].length;
    const open = source.indexOf("{", start);
    const end = matchBracket(source, open);
    const condition = source.slice(start, open);
    const body = source.slice(open + 1, end - 1);
    const names = stringLiterals(
      condition.match(/matches!\(self\.name\.as_str\(\),([\s\S]*?)\)/)?.[1] ?? "",
    );
    const prefix = condition.match(/self\.name\.starts_with\("([^"]+)"\)/)?.[1];
    const topic = condition.match(/args\["topic"\]\s*==\s*"([^"]+)"/)?.[1];
    if (!topic || !names.length) throw new Error("Unsupported conditional verb/topic policy");
    const writeClass = body
      .match(/binding::capability\(scope,\s*self\.class\s*==\s*Class::(\w+)\)/)?.[1]
      ?.toLowerCase();
    const caller = body.match(
      /if\s+caller\.scope\s*==\s*"([^"]+)"\s*\{\s*"([^"]+)"\s*\}\s*else\s*\{\s*"([^"]+)"/,
    );
    const constant = body.trim().match(/^return\s+"([^"]+)"\s*;/)?.[1];
    for (const verb of verbs) {
      const matchesPrefix = prefix && verb.name.startsWith(prefix);
      if (!matchesPrefix && !names.includes(verb.name)) continue;
      if (writeClass) {
        for (const scope of scopes) {
          const capability = boundGrants.get(
            `${scope.rustName === privateVariant}:${verb.class === writeClass}`,
          );
          grants[verb.name].push({
            ...(matchesPrefix ? {} : { topic }),
            collectionScope: scope.name,
            capability,
          });
        }
      } else if (caller) {
        grants[verb.name].push(
          { topic, callerScope: caller[1], capability: caller[2] },
          { topic, callerScope: "other", capability: caller[3] },
        );
      } else if (constant) {
        grants[verb.name].push({ topic, capability: constant });
      } else throw new Error("Unsupported conditional capability branch");
    }
    offset = end;
  }
  return grants;
}

export function describeConditionalGrant(rule) {
  return (
    [
      rule.topic && `topic=${rule.topic}`,
      rule.callerScope && `caller scope=${rule.callerScope}`,
      rule.collectionScope && `bound collection scope=${rule.collectionScope}`,
    ]
      .filter(Boolean)
      .join("; ") + ` → ${rule.capability}`
  );
}
