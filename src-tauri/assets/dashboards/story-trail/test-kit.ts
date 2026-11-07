// SPDX-License-Identifier: MIT
import {readFileSync} from 'node:fs';
const source=readFileSync('src-tauri/src/http/routes/dashboard_kit/kit.js','utf8');
export function trailTestKit(){
 const w=window as any;
 if(!w.codesense?.kit)window.eval(source);
 return w.codesense.kit;
}
