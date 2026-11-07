// SPDX-License-Identifier: MIT
import {readFileSync, writeFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
const read = name => readFileSync(new URL(name,import.meta.url),'utf8');
export function buildHtml(){
  const script = ['focus.mjs','intent-reader.mjs','rate-state.mjs','model.mjs','patches.mjs','trail-view.mjs','conversation.mjs','session-dom.mjs','changes-view.mjs','lines-view.mjs','panel-view.mjs','conversation-view.mjs','page.mjs'].map(name=>read(name).replace(/^import .*;\n/gm,'').replaceAll('export ','')).join('\n');
  return `<!doctype html>\n<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta name="codesense:capabilities" content="sessions:read accounts:manage context:read repo:read launch"><title>Story trail</title><style>\n${read('trail.css')}</style></head><body class="cs-app"><main id="trail"></main><aside id="drill-panel" hidden></aside><script>(()=>{\n${script}\nmountTrail();\n})();</script></body></html>\n`;
}
if(process.argv[1] === fileURLToPath(import.meta.url)) writeFileSync(new URL('index.html',import.meta.url),buildHtml());
