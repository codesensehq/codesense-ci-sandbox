// SPDX-License-Identifier: MIT
use std::path::{Component, PathBuf};

use crate::dashboard_stream::frame::{FrameMessage, FrameRecord};
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use tauri::Manager;
use tokio::fs::File;
use tokio_util::io::ReaderStream;

use crate::commands::git_sync::{read_baseline_blob, read_tree_blob};
use crate::http::AppState;

/// The frozen `window.codesense.version` string the `LINK_INTERCEPTOR_JS` shim exposes. The
/// shim is a raw string, so this constant is bound to it by a test; the MCP
/// live-data contract publishes it from here.
pub(crate) const PAGE_SDK_VERSION: &str = "4";

/// Additive SDK features a page can test with `codesense.has(name)`. They extend this list
/// instead of bumping [`PAGE_SDK_VERSION`]. The host sends them with the frame's granted
/// verb names in the `sdk` field of `codesense:slot-context`, and the live-data contract
/// publishes them; `pnpm gen:verbs` copies them for the host.
/// Architecture: never a verb name, so a verb's `has()` always follows the grant. `has()` is
/// informational; Rust still checks every call.
pub(crate) const PAGE_SDK_FEATURES: &[&str] = &[
    "window",
    "firstPaint",
    "lifecycle",
    "slotContext",
    "theme",
    "kitTokens",
    "virtualList",
];

/// JavaScript interceptor injected into HTML responses.
///
/// SW-LD-1: also carries the `window.codesense` bridge shim (last IIFE) — a
/// `postMessage` RPC: `window.codesense.invoke(command, args)` posts
/// `{token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:invoke', id, command, args}` to `window.parent` and resolves
/// when a matching `codesense:invoke-result` arrives. The shim holds no policy
/// (this server has no app-state); the PARENT (`html-viewer.tsx`) services every
/// allowlisted command for any live doc and replies `{ok:false}` for anything
/// else.
///
/// SW-LD-1: also forwards ⌘W — a focused iframe swallows the keydown so the
/// app's global Cmd+W (close active tab) never fires; the injected `keydown`
/// handler posts `codesense:close-tab` to the parent, which routes to the same
/// `closeActiveFileTab` primitive every other close affordance uses. Without
/// this, HTML doc tabs (unlike markdown, which renders in-app) can't be closed
/// with ⌘W while the doc has focus.
///
/// The window bubble listener forwards every unhandled Command, Ctrl or Option
/// chord and every function key as
/// `codesense:shortcut {key, code, metaKey, ctrlKey, shiftKey, altKey, editorTarget, textFieldTarget}`,
/// and a plain Escape as `codesense:key-escape`. The host matches them against the
/// whole shortcut registry (built-ins and dashboard commands) with the user's
/// current bindings and applies its normal UI gate.
/// Generic forwarding uses a macrotask: trusted events may run a microtask
/// checkpoint between listeners, before later page window handlers can prevent
/// the key. Recheck defaultPrevented after dispatch so those handlers keep it.
/// Composing and unmodified keys stay in the page (modified arrows are chords like
/// any other). Generic forwarding preserves browser defaults for copy, paste and undo.
/// Close tab (Command-W, a reserved chord) retains its dedicated path.
///
/// SW-SD-1: the shim IIFE also carries the snapshot channel. A parent→doc
/// `codesense:snapshot-request` is answered by `serializeSnapshot(undefined)`
/// (clone `documentElement`, strip EVERY `<script>` — the injected shim
/// included, plus scripts nested in `<template>` content — so the frozen DOM
/// renders identically offline) posted back as
/// `codesense:snapshot-result`. The doc-facing `window.codesense.snapshot(state)`
/// verb serializes the same way plus an inert `<script type="application/json"
/// id="codesense-snapshot-state">` sidecar (JSON with `<`→`\u003c` escaped so a
/// `</script>` in a value can't break the raw-text element — the twin of
/// `render_proxy_script`'s discipline), posts `codesense:snapshot` to the
/// parent, and resolves once the parent acks. Like `invoke`, the shim holds no
/// policy — the parent archives the snapshot.
///
/// Five responsibilities, all over the `window.parent` postMessage channel:
/// 1. Click interception — relative links to viewable files (.md, .html,
///    .htm) dispatch `codesense:navigate`; external links (explicit scheme)
///    dispatch `codesense:external-link`.
/// 2. Scroll reporting (SW-FX-5) — on scroll (throttled to one rAF) post
///    `codesense:scroll` with the current `window.scrollY` so the parent can
///    persist it across the iframe reload that a refresh-key bump triggers.
///    Reporting is suppressed (`_rs`) while a restore is in flight so the
///    restore's own `scrollTo` can't echo a clamped offset back to the
///    parent and erode the saved position (or zero it on a not-yet-laid-out
///    reload).
/// 3. Scroll restore (SW-FX-5) — on `codesense:restore-scroll` FROM
///    `window.parent` only, call `window.scrollTo(0, y)` and re-assert it
///    on the next animation frame so a reload whose height settles after
///    `load` (web fonts, async layout) still lands on the saved position
///    rather than clamping short. The `e.source === window.parent` check
///    prevents a nested/3rd-party frame from spoofing a restore; the finite
///    check rejects `NaN`/`Infinity` payloads.
///
/// A doc is themed and told when it is on screen by its host: the shim posts
/// `codesense:ready` once it runs, and the host answers (and later pushes)
/// `codesense:theme {theme:{scheme, tokens}}` and `codesense:visibility
/// {visible}`. The theme's tokens become CSS custom properties on the root, plus
/// `--cs-color-scheme`. A pane's root also takes `color-scheme` (`applyScheme`),
/// so a plain pane reads right on the surface it sits on; a centre doc's root
/// takes it only when the doc styles itself with `--cs-*`
/// (`window.__codesenseThemeAware`, set by the server), so a doc that declares
/// its own look keeps it. A snapshot leaves the pushed theme out. `codesense:sessions-changed` is a payloadless nudge that re-runs
/// `onSessions`, and `setBadge` posts `codesense:badge` for the host's chrome.
/// `agent`, `inbox`, `roles`, `updates` and `humanStatus` are the agents read model's
/// bridge verbs; `onAgentsChanged` / `onInboxChanged` run their callback on the
/// payloadless `codesense:agents-changed` / `codesense:inbox-changed` nudges.
/// `getNavSelection()` / `onNavSelectionChange(cb)` read the selection delivered
/// in `codesense:slot-context` after ready and on settled navigation changes.
/// A read resolves `null` rather than wait past 5 s; callbacks can unsubscribe.
/// `has(name)` answers from the `sdk {features, verbs}` that slot context carries: an SDK
/// feature, or a verb the frame's effective grant admits; false until the host has answered.
/// `context()`, `openFile(…)` and `setField(…)` are plain bridge verbs: the host
/// answers where the page lives, opens a file inside a known team content root,
/// and writes one frontmatter field.
///
/// Errors are forwarded, never swallowed: an uncaught script error, a failed
/// resource load (captured) or an unhandled promise rejection inside the doc
/// posts `codesense:doc-error {kind, message, source?, line?}` to the parent,
/// which a pane host shows in place.
///
/// Uses raw `getAttribute('href')` for classification — NOT `new URL()` /
/// `location.origin` — because the iframe sandbox (`allow-scripts` without
/// `allow-same-origin`) gives an opaque origin where `location.origin`
/// evaluates to `"null"`. For the same reason scroll state cannot live in
/// the iframe (`sessionStorage`/`localStorage` throw) — the parent holds it.
///
/// Keep extension list in sync with VIEWER_EXTENSIONS in
/// src/shared/lib/viewer-file-type.ts.
const LINK_INTERCEPTOR_JS: &str = r#"document.addEventListener('click',function(e){var a=e.target;while(a&&a.tagName!=='A')a=a.parentElement;if(!a)return;var raw=a.getAttribute('href');if(!raw)return;if(raw.charAt(0)==='#')return;if(raw.charAt(0)==='/'&&raw.charAt(1)==='/'){e.preventDefault();window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:external-link',href:raw},'*');return}if(/^[a-z][a-z0-9+.-]*:/i.test(raw)){e.preventDefault();window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:external-link',href:raw},'*');return}var p=raw.split(/[?#]/)[0];var ext=p.split('.').pop().toLowerCase();if(ext!=='md'&&ext!=='html'&&ext!=='htm')return;e.preventDefault();window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:navigate',href:raw,metaKey:e.metaKey,ctrlKey:e.ctrlKey},'*')});var _st,_rs;window.addEventListener('scroll',function(){if(_rs||_st)return;_st=requestAnimationFrame(function(){_st=0;if(_rs)return;window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:scroll',y:window.scrollY},'*')})},{passive:true});window.addEventListener('message',function(e){if(e.source!==window.parent)return;var d=e.data;if(!d||d.type!=='codesense:restore-scroll'||typeof d.y!=='number'||!isFinite(d.y))return;var y=d.y;_rs=true;window.scrollTo(0,y);requestAnimationFrame(function(){window.scrollTo(0,y);requestAnimationFrame(function(){_rs=false})})});function _csKeyBridge(e,hostOnly){if(e.defaultPrevented||e.isComposing)return;if(e.key==='Escape'&&!e.metaKey&&!e.ctrlKey&&!e.shiftKey&&!e.altKey){setTimeout(function(){if(!e.defaultPrevented||hostOnly)window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:key-escape'},'*')},0);return}if(!e.metaKey&&!e.ctrlKey&&!e.altKey&&!/^F\d{1,2}$/.test(e.key))return;if((e.metaKey||e.ctrlKey)&&!e.shiftKey&&!e.altKey&&(e.key==='w'||e.key==='W')){e.preventDefault();window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:close-tab'},'*');return}var editorTarget=!!(e.target&&e.target.closest&&e.target.closest('.cm-editor, .ProseMirror, [contenteditable]:not([contenteditable="false"])'));setTimeout(function(){if(!e.defaultPrevented||hostOnly)window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:shortcut',key:e.key,code:e.code,metaKey:e.metaKey,ctrlKey:e.ctrlKey,shiftKey:e.shiftKey,altKey:e.altKey,editorTarget:editorTarget,textFieldTarget:!!(e.target&&e.target.closest&&e.target.closest('input, textarea, select'))},'*')},0)}window.addEventListener('keydown',_csKeyBridge);window.addEventListener('focus',function(){window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:frame-focus'},'*')});(function(){var _n=0,_p={},_fp,_fpDone=false,_th=null,_thNone=false,_thOwn=false,_thKeys=[],_thWait=[],_thL=[],_vis=true,_visKnown=false,_visL=[],_intent=undefined,_intentL=[],_nav=null,_navKnown=false,_navL=[],_navWait=[],_slot=null,_slotL=[],_flushL=[],_teardownL=[],_focusL=[],_focusReturnL=[],_menuTurn=false,_menuSerial=0,_cmdL=[],_menuActionL=[],_sdk={features:[],verbs:[]},_rb=false,_rbQ=[];
var _textTarget=null,_textSerial=0,_textMenuId=0,_rowWarning=false,_menuPointerRange,_menuPointerText;
window.addEventListener('keydown',function(e){
  if(e.defaultPrevented||!(e.key==='ContextMenu'||(e.shiftKey&&e.key==='F10')))return;
  e.preventDefault();
  (document.activeElement||document.body).dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true}));
});
function _selectionAt(e){
  var selection=window.getSelection();
  if(!selection||selection.isCollapsed||!selection.rangeCount)return null;
  var range=selection.getRangeAt(0);
  if(e.clientX===0&&e.clientY===0){var active=document.activeElement;if(active&&active!==document.body&&!range.intersectsNode(active))return null;return range.cloneRange()}
  var rects=range.getClientRects();
  for(var i=0;i<rects.length;i++){var r=rects[i];if(e.clientX>=r.left&&e.clientX<=r.right&&e.clientY>=r.top&&e.clientY<=r.bottom)return range.cloneRange()}
  return null;
}
function _restoreText(t){
  if(t.field&&t.field.isConnected){
    t.field.focus();
    if(t.start!==null&&t.start!==undefined)try{t.field.setSelectionRange(t.start,t.end,t.direction)}catch(err){}
  }
  if(t.range){var selection=window.getSelection();selection.removeAllRanges();selection.addRange(t.range)}
}
window.addEventListener('pointerdown',function(e){
  if(e.button!==2)return;
  _menuPointerRange=_selectionAt(e);_menuPointerText=null;
  var field=e.target instanceof Element&&e.target.closest('input,textarea');
  if(field&&document.activeElement===field&&typeof field.selectionStart==='number'&&field.selectionStart!==field.selectionEnd)
    _menuPointerText={field:field,start:field.selectionStart,end:field.selectionEnd,direction:field.selectionDirection};
},true);
window.addEventListener('contextmenu',function(e){
  if(e.defaultPrevented){_menuPointerRange=undefined;_menuPointerText=null;_textTarget=null;return;}
  var target=e.target instanceof Element?e.target:null;
  if(e.clientX===0&&e.clientY===0&&document.activeElement instanceof Element)target=document.activeElement;
  var field=target&&target.closest('input,textarea,[contenteditable]:not([contenteditable="false"])');
  if(field&&field.tagName==='INPUT'&&!/^(text|search|url|tel|email|number|password)$/i.test(field.type))field=null;
  var range=(e.clientX===0&&e.clientY===0)||_menuPointerRange===undefined?_selectionAt(e):_menuPointerRange;
  _menuPointerRange=undefined;
  if(field&&'value' in field)range=null;
  var el=target&&target.closest('[data-codesense-row]'),row;
  if(el)try{row=JSON.parse(el.getAttribute('data-codesense-row'))}catch(err){}
  var kind=row&&row.kind,known=['agent','story','epic','file','workspace','group','conversation','message','attachment','role-queue'].indexOf(kind)>=0;
  if(kind&&!known&&!_rowWarning){_rowWarning=true;console.warn('codesense: unknown row kind "'+kind+'"; showing the dashboard menu')}
  var text=!!field||!!range;
  var password=!!field&&field.type==='password';
  var editable=!!field&&!field.disabled&&!field.readOnly&&field.getAttribute('aria-readonly')!=='true';
  var saved=_menuPointerText&&_menuPointerText.field===field?_menuPointerText:null;
  _menuPointerText=null;
  var start=saved?saved.start:field&&field.selectionStart,end=saved?saved.end:field&&field.selectionEnd;
  var hasSelection=!!range||(typeof start==='number'&&start!==end);
  var selection=window.getSelection();
  if(field&&typeof start!=='number'&&document.activeElement===field&&selection&&selection.toString())hasSelection=true;
  if(!range&&field&&!('value' in field)&&selection&&selection.rangeCount&&field.contains(selection.anchorNode))range=selection.getRangeAt(0).cloneRange();
  hasSelection=hasSelection||!!(range&&!range.collapsed);
  var textId=++_textSerial;
  _textTarget=text?{id:textId,field:field,range:range,start:start,end:end,direction:saved?saved.direction:field&&field.selectionDirection}:null;
  var rect={x:e.clientX,y:e.clientY,width:0,height:0};
  if(e.clientX===0&&e.clientY===0&&target){var box=target.getBoundingClientRect();rect={x:box.left,y:box.bottom,width:box.width,height:0}}
  var skip=_menuTurn,serial=_menuSerial,handled=false,prevent=e.preventDefault;
  e.preventDefault=function(){handled=true;prevent.call(e)};
  prevent.call(e);
  setTimeout(function(){
    e.preventDefault=prevent;
    if(handled||skip||serial!==_menuSerial){if(_textTarget&&_textTarget.id===textId)_textTarget=null;return;}
    _textMenuId=text?textId:0;
    window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:contextmenu',target:text?'text':known?'row':'frame',row:row,text:text?{editable:editable,hasSelection:hasSelection,password:password,hasText:!!(field?('value' in field?field.value:field.textContent):document.body.textContent)}:undefined,textId:textId,anchorId:(text&&field?field.id:el&&el.id)||(target&&target.id)||undefined,anchorRect:rect},'*');
  },0);
});
window.addEventListener('message',function(e){
  if(e.source!==window.parent)return;
  var d=e.data||{};
  if(d.type==='codesense:text-menu-closed'&&d.textId===_textMenuId){_textMenuId=0;return;}
  if(d.type==='codesense:child-rect'){
    var frames=document.querySelectorAll('iframe');
    for(var i=0;i<frames.length;i++){
      if(new URL(frames[i].src,location.href).origin!==d.origin)continue;
      var rect=frames[i].getBoundingClientRect();
      window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:child-rect-result',id:d.id,rect:{x:rect.left,y:rect.top,width:rect.width,height:rect.height}},'*');return;
    }
    window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:child-rect-result',id:d.id},'*');return;
  }
  if(d.type==='codesense:child-focus'){
    var frames=document.querySelectorAll('iframe');
    for(var i=0;i<frames.length;i++)if(new URL(frames[i].src,location.href).origin===d.origin){frames[i].focus();return}
  }
  if(d.type!=='codesense:text-edit'||!_textTarget||d.textId!==_textTarget.id)return;
  var t=_textTarget;
  _restoreText(t);
  if(['cut','copy','selectAll'].indexOf(d.action)>=0)document.execCommand(d.action);
  else if(d.action==='paste'&&typeof d.text==='string')document.execCommand('insertText',false,d.text);
  if(t.field){t.start=t.field.selectionStart;t.end=t.field.selectionEnd;t.direction=t.field.selectionDirection}
  var selected=window.getSelection();t.range=t.field&&'value' in t.field?null:selected&&selected.rangeCount?selected.getRangeAt(0).cloneRange():null;
});
function _csNow(){return typeof performance!=='undefined'&&performance.timeOrigin!==undefined?performance.timeOrigin+performance.now():Date.now()}function _off(list,cb){return function(){var i=list.indexOf(cb);if(i>=0)list.splice(i,1)}}function _applyTheme(t,own){var r=document.documentElement,k=(t&&typeof t.tokens==='object'&&t.tokens)||{},names=[];_thKeys.forEach(function(n){if(!(n in k))r.style.removeProperty(n)});for(var n in k)if(n.indexOf('--')===0&&typeof k[n]==='string'){r.style.setProperty(n,k[n]);names.push(n)}var sc=t.scheme==='light'?'light':'dark';r.style.setProperty('--cs-color-scheme',sc);_thKeys=names.concat('--cs-color-scheme');if(own){r.style.colorScheme=sc;_thOwn=true}else if(_thOwn){r.style.removeProperty('color-scheme');_thOwn=false}var tokens={};names.forEach(function(n){tokens[n]=k[n]});_th={scheme:sc,tokens:tokens};_thWait.splice(0).forEach(function(f){f(_th)});_thL.slice().forEach(function(cb){cb(_th)})}function stripScripts(root){root.querySelectorAll('script').forEach(function(s){s.remove()});root.querySelectorAll('template').forEach(function(t){stripScripts(t.content)})}function serializeSnapshot(state){var _dd=document.getElementById('codesense-dashboard-data');var _ddText=_dd?_dd.textContent:null;var clone=document.documentElement.cloneNode(true);clone.querySelectorAll('#codesense-initial-theme').forEach(function(s){s.remove()});stripScripts(clone);_thKeys.forEach(function(n){clone.style.removeProperty(n)});if(_thOwn)clone.style.removeProperty('color-scheme');if(clone.getAttribute('style')==='')clone.removeAttribute('style');var _host=clone.querySelector('head')||clone.querySelector('body')||clone;if(_ddText!==null){var dblob=document.createElement('script');dblob.type='application/json';dblob.id='codesense-dashboard-data';dblob.textContent=_ddText.replace(/</g,'\\u003c');_host.appendChild(dblob)}if(state!==undefined){var blob=document.createElement('script');blob.type='application/json';blob.id='codesense-snapshot-state';blob.textContent=JSON.stringify(state).replace(/</g,'\\u003c');_host.appendChild(blob)}return '<!doctype html>\n'+clone.outerHTML}function _csFP(){try{var el=document.getElementById('codesense-dashboard-data');return el?JSON.parse(el.textContent):null}catch(e){return null}}window.addEventListener('message',function(e){if(e.source!==window.parent)return;var d=e.data;if(!d)return;if(d.type==='codesense:invoke-result'||d.type==='codesense:snapshot-result'){var cb=_p[d.id];if(!cb)return;delete _p[d.id];if(d.ok)cb.res(d.data);else cb.rej(new Error(d.error||'codesense bridge error'));return}if(d.type==='codesense:flush'){var ack=function(){window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:flush-ack',id:d.id},'*')};if(!_flushL.length)ack();else Promise.allSettled(_flushL.slice().map(function(cb){return Promise.resolve().then(cb)})).then(ack);return}if(d.type==='codesense:menu-action'){_menuActionL.slice().forEach(function(cb){cb({itemId:d.itemId,row:d.row})});if(d.id)window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:menu-action-ack',id:d.id},'*');return}if(d.type==='codesense:command'){_cmdL.slice().forEach(function(cb){cb({id:d.id,args:d.args,dispatchId:d.dispatchId})});return}if(d.type==='codesense:teardown'){_teardownL.slice().forEach(function(cb){cb()});return}if(d.type==='codesense:rebind-pending'){_rb=true;window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:rebind-ack'},'*');return}if(d.type==='codesense:rebind'){if(typeof d.token==='string'&&window.__codesenseProxy){window.__codesenseProxy.token=d.token;window.__codesenseProxy.readCap=d.token}_rb=false;_rbQ.splice(0).forEach(function(f){f()});return}if(d.type==='codesense:hello'){if(!window.__csSlotSeen)window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:ready',retry:true,sentAt:_csNow()},'*');return}if(d.type==='codesense:ping'){window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:pong'},'*');return}function _csFocusDoc(){window.focus()}if(d.type==='codesense:focus-return'){if(_textTarget){var saved=_textTarget;_textTarget=null;_restoreText(saved);_focusReturnL.slice().forEach(function(cb){cb({anchorId:d.anchorId})});return}if(_focusReturnL.length)_focusReturnL.slice().forEach(function(cb){cb({anchorId:d.anchorId})});else{var anchor=d.anchorId&&document.getElementById(d.anchorId);if(anchor)anchor.focus({preventScroll:true});if(!anchor||document.activeElement!==anchor)_csFocusDoc()}return}if(d.type==='codesense:focus-in'){if(_focusL.length)_focusL.slice().forEach(function(cb){cb()});else if(!document.activeElement||document.activeElement===document.body||document.activeElement===document.documentElement){_csFocusDoc()}return}if(d.type==='codesense:slot-context'){window.__csSlotSeen=true;if(d.sdk&&typeof d.sdk==='object')_sdk={features:Array.isArray(d.sdk.features)?d.sdk.features:[],verbs:Array.isArray(d.sdk.verbs)?d.sdk.verbs:[]};_slot=d.context;_slotL.slice().forEach(function(cb){cb(_slot)});if(_slot.selection!==undefined){_nav=_slot.selection||null;_navKnown=true;_navWait.splice(0).forEach(function(f){f(_nav)});_navL.slice().forEach(function(cb){cb(_nav)})}return}if(d.type==='codesense:theme'){window.__csSlotSeen=true;if(d.theme){_applyTheme(d.theme,d.applyScheme!==false||window.__codesenseThemeAware===true)}else{_thNone=true;_thWait.splice(0).forEach(function(f){f(null)})}(typeof requestAnimationFrame==='function'?requestAnimationFrame:function(f){setTimeout(f,0)})(function(){window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:theme-applied'},'*')});return}if(d.type==='codesense:intent'){if(_intentL.length)_intentL.slice().forEach(function(cb){cb(d.intent)});else _intent=d.intent;return}if(d.type==='codesense:visibility'){var v=d.visible!==false;if(_visKnown&&v===_vis)return;_vis=v;_visKnown=true;_visL.slice().forEach(function(cb){cb(v)});return}if(d.type==='codesense:snapshot-request'){try{if(d.format&&document.readyState!=='complete')throw new Error('Document is still loading; wait for it to finish and retry');if(d.format&&d.expectedUrl&&d.expectedUrl.split('#')[0]!==location.href.split('#')[0])throw new Error('Document navigated; select the current document and retry');var h=d.format==='text'?(document.body?document.body.innerText:''):serializeSnapshot(undefined);var truncated=false;if(d.format){var bytes=new TextEncoder().encode(h);if(bytes.length>524288){var end=524288;while(end>0&&(bytes[end]&192)===128)end--;h=new TextDecoder().decode(bytes.subarray(0,end));truncated=true}}window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:snapshot-result',id:d.id,ok:true,html:d.format?undefined:h,content:d.format?h:undefined,capturedAt:new Date().toISOString(),truncated:truncated,sourceUrl:location.href},'*')}catch(err){window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:snapshot-result',id:d.id,ok:false,error:String((err&&err.message)||err)},'*')}}});window.codesense={get textMenuOpen(){return _textMenuId!==0},version:'4',has:function(n){return _sdk.features.indexOf(n)>=0||_sdk.verbs.indexOf(n)>=0},onFlush:function(cb){_flushL.push(cb);return _off(_flushL,cb)},onTeardown:function(cb){_teardownL.push(cb);return _off(_teardownL,cb)},onFocusIn:function(cb){_focusL.push(cb);return _off(_focusL,cb)},onFocusReturn:function(cb){_focusReturnL.push(cb);return _off(_focusReturnL,cb)},showMenu:function(a){_menuSerial++;_menuTurn=true;setTimeout(function(){_menuTurn=false},0);return this.invoke('showMenu',a||{})},showPopover:function(a){return this.invoke('showPopover',a||{})},onSlotContext:function(cb){_slotL.push(cb);if(_slot)cb(_slot);return _off(_slotL,cb)},focusOut:function(a){return this.invoke('focusOut',a||{})},focusTerminal:function(){return this.invoke('focusTerminal',{})},commands:{register:function(ids){return window.codesense.invoke('commands.register',{ids:ids})},list:function(){return window.codesense.invoke('commands.list',{})},run:function(id,args){return window.codesense.invoke('commands.run',{id:id,args:args})}},onMenuAction:function(cb){_menuActionL.push(cb);function advertise(){window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:menu-actions',enabled:_menuActionL.length>0},'*')}advertise();return function(){_off(_menuActionL,cb)();advertise()}},onCommand:function(cb){_cmdL.push(cb);return _off(_cmdL,cb)},ready:function(){window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:live'},'*')},get firstPaint(){if(_fpDone)return _fp;var v=_csFP();if(v!==null){_fp=v;_fpDone=true;return v}return window.__codesenseFirstPaint||null},invoke:function(command,args){return new Promise(function(res,rej){function send(){var id=++_n;_p[id]={res:res,rej:rej};window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:invoke',id:id,command:command,args:args,sentAt:_csNow()},'*')}if(_rb)_rbQ.push(send);else send()})},act:function(v,a){return this.invoke(v,a)},routineResult:function(a){return this.invoke('routineResult',a)},routineRefresh:function(a){return this.invoke('routineRefresh',a)},routineStatus:function(a){return this.invoke('routineStatus',a)},routineCancel:function(a){return this.invoke('routineCancel',a)},onRoutineResult:function(cb){function changed(e){if(e.source===window.parent&&e.data&&e.data.type==='codesense:routines-changed')cb()}window.addEventListener('message',changed);return function(){window.removeEventListener('message',changed)}},readWorkspace:function(a){return this.invoke('readWorkspace',a)},queryContext:function(q){return this.invoke('queryContext',q||{})},subscribeContext:function(q,cb){var self=this,gen=0,closed=false,so=null;function run(){var my=++gen;self.invoke('queryContext',q||{}).then(function(r){if(!closed&&my===gen){so=(r&&r.scopeOrgs)||null;cb(r,null)}},function(err){if(!closed&&my===gen)cb(null,err)})}function changed(e){var d=e.data;if(e.source!==window.parent||!d||d.type!=='codesense:context-changed')return;if(!so||d.truncated||d.allOrgs||(d.orgs||[]).some(function(o){return so.indexOf(o)>=0}))run()}window.addEventListener('message',changed);run();return function(){closed=true;window.removeEventListener('message',changed)}},snapshot:function(state){return new Promise(function(res,rej){var id=++_n;var h;try{h=serializeSnapshot(state)}catch(err){rej(err);return}_p[id]={res:res,rej:rej};window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:snapshot',id:id,html:h},'*')})},theme:function(){return _th||_thNone?Promise.resolve(_th):new Promise(function(res){_thWait.push(res)})},onTheme:function(cb){_thL.push(cb);if(_th)cb(_th);return _off(_thL,cb)},onIntent:function(cb){_intentL.push(cb);if(_intent!==undefined){var intent=_intent;_intent=undefined;cb(intent)}return _off(_intentL,cb)},onVisibility:function(cb){_visL.push(cb);if(_visKnown)cb(_vis);return _off(_visL,cb)},prefs:function(){return this.invoke('prefs')},setPrefs:function(p){return this.invoke('setPrefs',p||{})},getNavSelection:function(){if(_navKnown)return Promise.resolve(_nav);return new Promise(function(res){_navWait.push(res);setTimeout(function(){var i=_navWait.indexOf(res);if(i<0)return;_navWait.splice(i,1);res(null)},5000)})},onNavSelectionChange:function(cb){_navL.push(cb);if(_navKnown)cb(_nav);return _off(_navL,cb)},sessions:function(){return this.invoke('sessions')},agents:function(){return this.invoke('agents')},agent:function(a){return this.invoke('agent',a)},inbox:function(a){return this.invoke('inbox',a||{})},inboxRollup:function(){return this.invoke('inboxRollup')},roles:function(a){return this.invoke('roles',a||{})},updates:function(a){return this.invoke('updates',a||{})},humanStatus:function(){return this.invoke('humanStatus')},onAgentsChanged:function(cb){function changed(e){if(e.source===window.parent&&e.data&&e.data.type==='codesense:agents-changed')cb()}window.addEventListener('message',changed);return function(){window.removeEventListener('message',changed)}},onInboxChanged:function(cb){function changed(e){if(e.source===window.parent&&e.data&&e.data.type==='codesense:inbox-changed')cb(e.data)}window.addEventListener('message',changed);return function(){window.removeEventListener('message',changed)}},context:function(){return this.invoke('context')},openFile:function(t,o){var a=t&&typeof t==='object'?t:{path:t};return this.invoke('openFile',{path:a.path,team:a.team,newTab:!!((o&&o.newTab)||a.newTab),preview:o&&o.preview!==undefined?o.preview:a.preview})},setField:function(a){return this.invoke('setField',a)},setBadge:function(b){if(b!==null&&b!==undefined&&(typeof b!=='object'||typeof b.count!=='number'||!isFinite(b.count)||b.count<0))throw new TypeError('setBadge: pass {count, severity?} or null');if(b&&b.severity!==undefined&&b.severity!=='info'&&b.severity!=='warning'&&b.severity!=='error')throw new TypeError("setBadge: severity is 'info', 'warning' or 'error'");window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:badge',badge:b?{count:Math.floor(b.count),severity:b.severity||'info'}:null},'*')},sprints:function(){var px=window.__codesenseProxy;if(!px||!px.readCap)return Promise.reject(new Error('unavailable: no read capability'));return fetch(px.base+'/api/v1/sprints',{headers:{'x-codesense-proxy-token':px.readCap}}).then(function(r){if(r.status===401)return Promise.reject(new Error('expired'));if(!r.ok)return Promise.reject(new Error('codesense-sprints-'+r.status));return r.json()})},taxonomy:function(){var px=window.__codesenseProxy;if(!px||!px.readCap)return Promise.reject(new Error('unavailable: no read capability'));return fetch(px.base+'/api/v1/taxonomy',{headers:{'x-codesense-proxy-token':px.readCap}}).then(function(r){if(r.status===401)return Promise.reject(new Error('expired'));if(!r.ok)return Promise.reject(new Error('codesense-taxonomy-'+r.status));return r.json()})}};window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:ready',sentAt:_csNow()},'*')})();window.addEventListener('error',function(e){var t=e&&e.target;var m=t&&t!==window&&t.tagName?'failed to load '+(t.getAttribute('src')||t.getAttribute('href')||t.tagName.toLowerCase()):(e&&e.message)||'script error';window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:doc-error',kind:'error',message:String(m),source:(e&&e.filename)||'',line:(e&&e.lineno)||0},'*')},true);window.addEventListener('unhandledrejection',function(e){var r=e&&e.reason;window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:doc-error',kind:'rejection',message:String((r&&r.message)||r)},'*')});
(function(){
  var cs=window.codesense,before=null,known=false,state=null,waiting=[],warned={},frozen=false,candidate=!!(window.location&&new URLSearchParams(window.location.search).get('codesenseCandidate')==='1'),pendingFocus=null,savedFocus=null;
  function post(data){data.token=window.__codesenseProxy&&window.__codesenseProxy.token;window.parent.postMessage(data,'*')}
  function warn(id,message){if(warned[id])return;warned[id]=true;console.warn(message)}
  if(typeof HTMLElement!=='undefined'){
    var focus=HTMLElement.prototype.focus;
    HTMLElement.prototype.focus=function(options){
      if(candidate){pendingFocus=this;post({type:'codesense:focus-suppressed'});return}
      return focus.call(this,options);
    };
  }
  ['keydown','keyup','beforeinput','cut','paste','compositionstart','compositionupdate','compositionend','submit','pointerdown','pointerup','click','dblclick','contextmenu','wheel','touchstart','touchmove','drop'].forEach(function(type){
    document.addEventListener(type,function(event){
      if(!frozen&&!candidate)return;
      if(type==='keydown'||type==='keyup'){
        if(event.key==='Tab'||event.key==='Escape'){
          event.stopImmediatePropagation();
          if(type==='keydown'&&event.key==='Escape')_csKeyBridge(event,true);
          return;
        }
        var editingChord=(event.metaKey||event.ctrlKey)&&/^[xvzy]$/i.test(event.key);
        var hostChord=event.metaKey||event.ctrlKey||event.altKey||/^F\d{1,2}$/.test(event.key);
        if(type==='keydown'&&!event.isComposing&&!editingChord&&hostChord)_csKeyBridge(event,true);
      }
      event.preventDefault();event.stopImmediatePropagation();
    },{capture:true,passive:false});
  });
  document.addEventListener('focusin',function(event){
    if(!candidate)return;
    pendingFocus=event.target;
    if(pendingFocus&&pendingFocus.blur)pendingFocus.blur();
    post({type:'codesense:focus-suppressed'});
  },true);
  cs.onBeforeReload=function(fn){before=fn;return function(){if(before===fn)before=null}};
  cs.reloadState=function(){return known?Promise.resolve(state):new Promise(function(resolve){waiting.push(resolve)})};
  window.addEventListener('message',function(event){
    if(event.source!==window.parent||!event.data)return;
    var d=event.data;
    if(d.type==='codesense:freeze'){
      if(d.frozen&&!frozen)savedFocus=document.activeElement;
      frozen=d.frozen===true;return;
    }
    if(d.type==='codesense:restore-focus'){
      if(savedFocus&&savedFocus.isConnected)savedFocus.focus({preventScroll:true});
      if(!savedFocus||document.activeElement!==savedFocus)window.focus();
      return;
    }
    if(d.type==='codesense:presentation'){
      candidate=d.shown!==true;
      if(!candidate){
        var target=pendingFocus;pendingFocus=null;
        if(d.focusAllowed&&target&&target.isConnected)target.focus({preventScroll:true});
      }
      return;
    }
    if(d.type==='codesense:teardown'){pendingFocus=null;savedFocus=null;return}
    if(d.type==='codesense:reload-state'){
      if(known)return;known=true;state=d.state==null?null:d.state;waiting.splice(0).forEach(function(resolve){resolve(state)});return;
    }
    if(d.type==='codesense:reload-warning'){warn(d.id,d.warning);return}
    if(d.type!=='codesense:before-reload')return;
    if(!before){post({type:'codesense:before-reload-result',id:d.id,state:null});return}
    var done=false,started=performance.now();
    function finish(value,warning){
      if(done)return;done=true;clearTimeout(timer);
      if(warning)warn(d.id,warning);
      post({type:'codesense:before-reload-result',id:d.id,state:warning?null:value,warning:warning});
    }
    var timer=setTimeout(function(){finish(null,'beforeReload snapshot exceeded 100 ms; dropped')},100);
    Promise.resolve().then(before).then(function(value){
      if(done)return;
      if(performance.now()-started>100){finish(null,'beforeReload snapshot exceeded 100 ms; dropped');return}
      try{
        var json=JSON.stringify(value==null?null:value);
        if(new TextEncoder().encode(json).length>65536){finish(null,'beforeReload snapshot exceeded 64 KB; dropped');return}
        finish(JSON.parse(json));
      }catch(error){finish(null,'beforeReload snapshot was not JSON; dropped')}
    },function(){finish(null,'beforeReload snapshot hook failed; dropped')});
  });
})();
"#;

/// The data reads the bridge shim adds to `window.codesense` (injected right after
/// [`LINK_INTERCEPTOR_JS`]): `routines()`, `routine({key})`, `routineRuns({key, limit?,
/// before?})`, `routineRun({runId})`, `runs({limit?})`, `run({runId})`, `runEvidence({runId,
/// storyId?})`, `runWorkProduct({runId})`, `worktrees()`, `machines()`, `sessionUsage({...})`, `modelRates()` and
/// `canPasteToTerminal()` invoke the bridge read verbs of the same names (each its MCP twin's or Tauri command's shape);
/// `onRoutinesChanged` / `onMachinesChanged` / `onWorktreesChanged` / `onPasteTargetChanged` run their
/// callback on the payloadless `codesense:routines-changed` / `codesense:machines-changed`
/// / `codesense:worktrees-changed` / `codesense:paste-target-changed` nudges the host pushes, and return an unsubscribe.
const LIVE_HUB_JS: &str = include_str!("live_hub.js");

/// Puts every registry verb on `window.codesense` as a method that invokes it, after the
/// shim, data reads and live hub, so a hand-written method of the same name wins. Generated
/// by `pnpm gen:verbs` with `dashboard_kit/codesense.d.ts`, which declares the same methods.
const VERB_METHODS_JS: &str = include_str!("verb_methods.generated.js");

const DATA_READS_JS: &str = r#"(function(){var c=window.codesense;if(!c)return;function on(t){return function(cb){function h(e){if(e.source===window.parent&&e.data&&e.data.type===t)cb()}window.addEventListener('message',h);return function(){window.removeEventListener('message',h)}}}var m={routines:function(){return c.invoke('routines')},routine:function(a){return c.invoke('routine',a)},routineRuns:function(a){return c.invoke('routineRuns',a)},routineRun:function(a){return c.invoke('routineRun',a)},runs:function(a){return c.invoke('runs',a||{})},run:function(a){return c.invoke('run',a)},runEvidence:function(a){return c.invoke('runEvidence',a)},runWorkProduct:function(a){return c.invoke('runWorkProduct',a)},worktrees:function(){return c.invoke('worktrees')},machines:function(){return c.invoke('machines')},sessionRead:function(a){return c.invoke('sessionRead',a)},whyLine:function(a){return c.invoke('whyLine',a)},setup:function(){return c.invoke('setup')},storyChanges:function(a){return c.invoke('storyChanges',a)},sessionUsage:function(a){return c.invoke('sessionUsage',a||{})},modelRates:function(){return c.invoke('modelRates')},canPasteToTerminal:function(){return c.invoke('canPasteToTerminal')},onRoutinesChanged:on('codesense:routines-changed'),onMachinesChanged:on('codesense:machines-changed'),onWorktreesChanged:on('codesense:worktrees-changed'),onPasteTargetChanged:on('codesense:paste-target-changed')};for(var k in m)c[k]=m[k]})();"#;

#[derive(Deserialize)]
pub struct ViewerQuery {
    /// Host-resolved presentation for the initial document; never a capability.
    pub theme: Option<String>,
    /// Cache-bust parameter — ignored by server, forces browser to reload.
    #[allow(dead_code)]
    pub v: Option<String>,
    /// SW-GS-10 — read the blob from THIS tree OID instead of the origin/main
    /// publish baseline. The conflict redline passes the immutable snapshot
    /// (`oursTreeOid`) and the merged tree so the resolved-vs-ours after-view
    /// renders from git objects, no disk write. Absent ⇒ the baseline (default).
    pub tree: Option<String>,
}

/// Decode the base64url-encoded context-repo root segment to its UTF-8 path
/// string. The single decode site shared by `resolve_viewer_path`, the read-
/// token content-root (`serve_file`), and the baseline blob root
/// (`serve_baseline_file`) — so the three never drift and one request decodes
/// the value once per use, not twice.
fn decode_viewer_root(root_b64url: &str) -> Result<String, StatusCode> {
    let bytes = URL_SAFE_NO_PAD
        .decode(root_b64url)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    String::from_utf8(bytes).map_err(|_| StatusCode::BAD_REQUEST)
}

/// Decode the base64url root segment and join with the relative path,
/// normalizing away `.` and `..` components.
fn resolve_viewer_path(root_b64url: &str, rel_path: &str) -> Result<PathBuf, StatusCode> {
    let root_str = decode_viewer_root(root_b64url)?;

    let joined = PathBuf::from(root_str).join(rel_path);
    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    Ok(normalized)
}

/// Inject the registered frame credential. The SDK's two transport lanes
/// use the same identity; JSON encoding and escaping protect the script body.
fn render_proxy_script(frame_token: &str, providers_json: &str) -> String {
    let token = serde_json::to_string(frame_token).unwrap_or_else(|_| "\"\"".to_string());

    let providers = if providers_json.is_empty() {
        "{}"
    } else {
        providers_json
    };
    let body = format!(
        "window.__codesenseProxy={{base:location.origin,token:{token},readCap:{token},providers:{providers}}}"
    );
    let body = body.replace('<', "\\u003c");
    format!("<script>{body}</script>")
}

/// Keep paths and private document locations out of third-party referrers.
fn html_doc_response(content_type: String, body: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::REFERRER_POLICY, "no-referrer".to_string()),
        ],
        body,
    )
        .into_response()
}

/// Serve a file from a context repo directory for the HTML viewer iframe.
///
/// The `root_b64url` segment is the context repo root path, base64url-encoded
/// (RFC 4648 §5, no padding). The `*path` wildcard is the relative file path
/// within that root.
///
/// - Validates the resolved absolute path via `validate_context_path`.
/// - Detects Content-Type via `mime_guess`.
/// - Streams the file via `tokio::fs::File` + `ReaderStream`.
#[utoipa::path(
    get,
    path = "/viewer/{root_b64url}/{path}",
    tag = "viewer",
    params(
        ("root_b64url" = String, Path, description = "Base64url-encoded context repo root path"),
        ("path" = String, Path, description = "Relative file path within the context repo root"),
        ("v" = Option<String>, Query, description = "Cache-bust version (ignored by server)")
    ),
    responses(
        (status = 200, description = "File content streamed with detected Content-Type"),
        (status = 403, description = "Path is outside allowed context directories"),
        (status = 404, description = "File not found")
    )
)]
pub async fn serve_file(
    State(state): State<AppState>,
    Path((root_b64url, rel_path)): Path<(String, String)>,
    Query(_query): Query<ViewerQuery>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    serve_file_request(
        &state.app_handle,
        &root_b64url,
        &rel_path,
        &headers,
        _query.theme.as_deref(),
    )
    .await
}

async fn serve_frame_file(
    file: &std::path::Path,
    frame: &FrameRecord,
    headers: &HeaderMap,
    theme: Option<&str>,
    first_paint: impl FnOnce(&[u8]) -> String,
    validate_context: impl FnOnce(&std::path::Path) -> Result<(), StatusCode>,
) -> Result<Response, StatusCode> {
    if !frame.allows_path(file) {
        return Err(StatusCode::FORBIDDEN);
    }
    let real = file.canonicalize().map_err(|_| StatusCode::NOT_FOUND)?;
    validate_context(&real)?;
    let registered = std::path::Path::new(&frame.document_path)
        .canonicalize()
        .ok()
        .as_ref()
        == Some(&real);
    let embedded_navigation = headers
        .get("sec-fetch-dest")
        .is_some_and(|d| d == "iframe" || d == "frame");
    if embedded_navigation && !registered {
        return Err(StatusCode::FORBIDDEN);
    }
    let mime = mime_guess::from_path(file).first_or_octet_stream();
    let content_type = mime.to_string();
    if mime.type_() == "text" && mime.subtype() == "html" {
        let contents = tokio::fs::read(&real)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let body = if registered {
            prepend_initial_theme(
                frame_html_body(frame, &contents, &first_paint(&contents)),
                theme,
                matches!(
                    frame.grant.slot.as_str(),
                    "dock" | "file-nav" | "right-pane"
                ) || theme_aware(&contents),
            )
        } else {
            contents
        };
        Ok(html_doc_response(content_type, body))
    } else {
        let file = File::open(&real)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        // Native WebKit media needs a finite response length, including when
        // the file is streamed rather than buffered like a baseline blob.
        let length = file
            .metadata()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .len();
        Ok((
            [
                (header::CONTENT_TYPE, content_type),
                (header::CONTENT_LENGTH, length.to_string()),
            ],
            Body::from_stream(ReaderStream::new(file)),
        )
            .into_response())
    }
}

// Run before authored scripts. Relative data reads need an explicit credential:
// third-party cookie restrictions also apply to same-origin fetch inside a frame.
// Popups remain allowed, but cannot borrow the opener's SDK or capability.
const FRAME_ENV_JS: &str = r#"(function(){
  // Read per request: a re-bound frame (the dock after a team switch) swaps its token.
  var proxy=window.__codesenseProxy, token=function(){return proxy.token}, nativeFetch=window.fetch.bind(window);
  window.fetch=function(input,init){
    var request=new Request(input,init);
    if(new URL(request.url).origin===location.origin){
      var headers=new Headers(request.headers);
      if(!headers.has('x-codesense-proxy-token'))headers.set('x-codesense-proxy-token',token());
      request=new Request(request,{headers:headers});
    }
    return nativeFetch(request);
  };
  var open=XMLHttpRequest.prototype.open, send=XMLHttpRequest.prototype.send;
  var sameOrigin=new WeakMap(), hasToken=new WeakSet(), setHeader=XMLHttpRequest.prototype.setRequestHeader;
  XMLHttpRequest.prototype.open=function(method,url){
    sameOrigin.set(this,new URL(url,location.href).origin===location.origin);
    hasToken.delete(this);
    return open.apply(this,arguments);
  };
  XMLHttpRequest.prototype.setRequestHeader=function(name,value){
    if(String(name).toLowerCase()==='x-codesense-proxy-token')hasToken.add(this);
    return setHeader.apply(this,arguments);
  };
  XMLHttpRequest.prototype.send=function(){
    if(sameOrigin.get(this)&&!hasToken.has(this))this.setRequestHeader('x-codesense-proxy-token',token());
    return send.apply(this,arguments);
  };
  var popup=window.open.bind(window);
  window.open=function(url,target,features){
    var safe=String(features||'').split(',').filter(function(f){return !/^\s*(noopener|noreferrer)\b/i.test(f)});
    safe.push('noopener');
    return popup(url,target,safe.join(','));
  };
  document.addEventListener('click',function(e){
    var link=e.target.closest&&e.target.closest('a[target],area[target]');
    if(link&&!/^_(self|parent|top)$/i.test(link.target))link.relList.add('noopener');
  },true);
  document.addEventListener('submit',function(e){
    var form=e.target;
    if(form.target&&!/^_(self|parent|top)$/i.test(form.target))form.relList.add('noopener');
  },true);
})();"#;

fn frame_first_paint<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    frame: &FrameRecord,
    html: &[u8],
) -> String {
    let Some(hub) = app.try_state::<crate::live_hub::LiveHub>() else {
        return String::new();
    };
    let Some(key) = crate::live_hub::view_key::resolve(&frame.grant) else {
        return String::new();
    };
    match crate::live_hub::LiveHub::snapshot(&hub, &key, &first_paint_topic(frame, html)) {
        Ok(snapshot) => first_paint_script(frame, &snapshot),
        Err(_) => String::new(),
    }
}

/// The topic a page's first paint carries: its `codesense:first-paint` meta, else the slot's
/// default (the dock's roster is `agentList`; every other slot paints `sessions`).
fn first_paint_topic(frame: &FrameRecord, html: &[u8]) -> String {
    let head = String::from_utf8_lossy(&html[..html.len().min(64 * 1024)]);
    crate::panes::pane_meta(&head, "codesense:first-paint")
        .filter(|topic| !topic.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if frame.grant.slot == "dock" {
                "agentList"
            } else {
                "sessions"
            }
            .into()
        })
}

fn first_paint_script(
    frame: &FrameRecord,
    snapshot: &crate::live_hub::projection::Snapshot,
) -> String {
    if crate::verbs::check_frame_capability(
        frame,
        "subscribe",
        &serde_json::json!({"topic": snapshot.topic}),
    )
    .is_err()
    {
        return String::new();
    }
    let data = serde_json::to_string(snapshot)
        .unwrap_or_default()
        .replace('<', "\\u003c");
    format!("window.__codesenseFirstPaint={data};")
}

/// Theme values come from the host's resolved surface, before any external kit can load.
fn prepend_initial_theme(body: Vec<u8>, theme: Option<&str>, apply_surface: bool) -> Vec<u8> {
    let Some(theme) = theme
        .filter(|value| value.len() <= 32 * 1024)
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
    else {
        return body;
    };
    let Some(scheme) = theme["scheme"]
        .as_str()
        .filter(|v| *v == "dark" || *v == "light")
    else {
        return body;
    };
    let Some(tokens) = theme["tokens"].as_object() else {
        return body;
    };
    let mut css = format!(
        "<style id=\"codesense-initial-theme\">:root:root{{--cs-color-scheme:{scheme};color-scheme:var(--cs-color-scheme,{scheme});"
    );
    for (name, value) in tokens {
        let Some(value) = value.as_str() else {
            continue;
        };
        if name.starts_with("--cs-")
            && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            && !value.contains([';', '{', '}', '<', '>', '\n', '\r'])
        {
            css.push_str(name);
            css.push(':');
            css.push_str(value);
            css.push(';');
        }
    }
    css.push('}');
    if apply_surface {
        css.push_str("html,body{background:var(--cs-bg);color:var(--cs-fg)}");
    }
    css.push_str("</style>");
    let mut result = css.into_bytes();
    result.extend(body);
    result
}

fn frame_html_body(frame: &FrameRecord, contents: &[u8], extra_js: &str) -> Vec<u8> {
    let mut proxy = render_proxy_script(&frame.token, "{}");
    proxy.push_str(&format!("<script>{FRAME_ENV_JS}</script>"));
    if frame.policy.blocks_service_workers() {
        proxy.push_str("<script>if(navigator.serviceWorker){Object.defineProperty(navigator.serviceWorker,'register',{value:function(){return Promise.reject(new Error('denied: service workers'))}})}</script>");
    }
    live_doc_body_for_host(proxy, contents, extra_js, frame.parent_frame.is_some())
}

/// What the live document renderer assembles, given the proxy handle script: the shim, the
/// dashboard kit (tokens, `cs-` components, `window.codesense.kit`), a mark that
/// the doc styles itself with the app's `--cs-*` tokens when it does, then any
/// `extra` host script. The doc's own bytes follow unchanged.
/// The e2e-webdriver build only (SV-PLG-46): answers `codesense:probe` from the host with what the
/// page had applied at that moment, so a real-app run can tell a late stylesheet or theme from a
/// host-side gap. Same sender check as the other shim handlers (`e.source` is the parent); a release
/// build does not compile it, so the shim it serves has no probe.
#[cfg(feature = "e2e-webdriver")]
const PROBE_JS: &str = r#"(function(){window.addEventListener('message',function(e){if(e.source!==window.parent)return;var d=e.data;if(!d||d.type!=='codesense:probe')return;var sheets=[];[].forEach.call(document.querySelectorAll('link[rel~=stylesheet]'),function(l){sheets.push({href:l.href,hasSheet:!!l.sheet})});var paint={};try{performance.getEntriesByType('paint').forEach(function(p){paint[p.name]=Math.round(p.startTime)})}catch(err){}var root=document.documentElement,body=document.body;window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:probe-result',id:d.id,readyState:document.readyState,styleSheets:document.styleSheets.length,sheets:sheets,bodyBackground:body?getComputedStyle(body).backgroundColor:null,rootBackground:getComputedStyle(root).backgroundColor,colorScheme:getComputedStyle(root).colorScheme,theme:root.dataset.theme||null,initialTheme:!!document.getElementById('codesense-initial-theme'),paint:paint},'*')})})();"#;

fn live_doc_body(proxy: String, contents: &[u8], extra_js: &str) -> Vec<u8> {
    live_doc_body_for_host(proxy, contents, extra_js, false)
}

fn live_doc_body_for_host(proxy: String, contents: &[u8], extra_js: &str, child: bool) -> Vec<u8> {
    let mut prefix = proxy;
    prefix.push_str(&format!(
        "<script>{LINK_INTERCEPTOR_JS}{DATA_READS_JS}{LIVE_HUB_JS}{VERB_METHODS_JS}</script>"
    ));
    prefix.push_str(&super::dashboard_kit::kit_tags());
    if theme_aware(contents) {
        prefix.push_str("<script>window.__codesenseThemeAware=true</script>");
    }
    if !extra_js.is_empty() {
        prefix.push_str(&format!("<script>{extra_js}</script>"));
    }
    #[cfg(feature = "e2e-webdriver")]
    prefix.push_str(&format!("<script>{PROBE_JS}</script>"));
    if child {
        prefix = prefix.replace("window.parent", "window.top");
    }
    let mut body = prefix.into_bytes();
    body.extend_from_slice(contents);
    body
}

/// Whether a doc styles itself with the app's theme tokens (`--cs-…`, or the kit's
/// `cs-app` page class), so it can take the app's `color-scheme` in a centre tab.
fn theme_aware(contents: &[u8]) -> bool {
    contents.windows(5).any(|w| w == b"--cs-") || contents.windows(6).any(|w| w == b"cs-app")
}

/// Resolve `rel_path` inside the `panes/` dir `panes_b64url` names, refusing
/// any path that normalizes outside it.
fn resolve_pane_path(panes_b64url: &str, rel_path: &str) -> Result<PathBuf, StatusCode> {
    let panes_dir = PathBuf::from(decode_viewer_root(panes_b64url)?);
    if panes_dir.file_name() != Some(std::ffi::OsStr::new(crate::context_repo::PANES_DIR))
        && !crate::panes::install::is_install_dir(&panes_dir)
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let resolved = resolve_viewer_path(panes_b64url, rel_path)?;
    if !resolved.starts_with(&panes_dir) || resolved == panes_dir {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(resolved)
}

/// Whether `real`, the resolved target of the pane path `file` that passes
/// through a link, may be served: it stays inside the `panes/` dir `file` is in,
/// or it is itself a context file.
#[cfg(test)]
fn link_target_allowed<R: tauri::Runtime>(
    file: &std::path::Path,
    real: &std::path::Path,
    app: &tauri::AppHandle<R>,
) -> bool {
    link_target_allowed_for(file, real, app, &Default::default())
}

fn link_target_allowed_for<R: tauri::Runtime>(
    file: &std::path::Path,
    real: &std::path::Path,
    app: &tauri::AppHandle<R>,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> bool {
    let panes_dir = file
        .ancestors()
        .find(|dir| dir.file_name() == Some(std::ffi::OsStr::new(crate::context_repo::PANES_DIR)));
    let in_panes = panes_dir
        .and_then(|dir| dir.canonicalize().ok())
        .is_some_and(|dir| real.starts_with(dir));
    in_panes || crate::commands::files::validate_context_path_for(real, app, scope).is_ok()
}

/// Serve a custom right pane, or one of its relative assets, from a `panes/`
/// dir. A pane lives beside `content/`, not in it, so it has its own route: the
/// file comes from `panes_b64url` while the SDK read capability is bound to
/// `content_b64url`, the active team's content, so a pane reads the same
/// stories a dashboard in that team does. A relative `lib/chart.js` resolves
/// under the same `panes/` dir.
#[utoipa::path(
    get,
    path = "/pane/{content_b64url}/{panes_b64url}/{path}",
    tag = "viewer",
    params(
        ("content_b64url" = String, Path, description = "Base64url-encoded content root the pane's reads resolve against"),
        ("panes_b64url" = String, Path, description = "Base64url-encoded panes/ dir holding the pane"),
        ("path" = String, Path, description = "Path within the panes/ dir: `<id>.html` or an asset"),
        ("v" = Option<String>, Query, description = "Cache-bust version (ignored by server)")
    ),
    responses(
        (status = 200, description = "Pane or asset streamed with detected Content-Type"),
        (status = 403, description = "Path is outside the panes/ dir or the context directories"),
        (status = 404, description = "File not found")
    )
)]
pub async fn serve_pane(
    State(state): State<AppState>,
    Path((content_b64url, panes_b64url, rel_path)): Path<(String, String, String)>,
    Query(_query): Query<ViewerQuery>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    serve_pane_request(
        &state.app_handle,
        &content_b64url,
        &panes_b64url,
        &rel_path,
        &headers,
        _query.theme.as_deref(),
    )
    .await
}

/// Serve a file's PUBLISH-BASELINE version (working-tree vs baseline "before"
/// side) for the SW-GS-3 image / HTML diff toggle. Same `root_b64url`/`*path`
/// shape as [`serve_file`], but `root_b64url` is the context repo's git root
/// (`org_root`) and the bytes come from the baseline git blob, not the working
/// tree. 404 when the path isn't in the baseline (i.e. an added file has no
/// "before").
#[utoipa::path(
    get,
    path = "/viewer-baseline/{root_b64url}/{path}",
    tag = "viewer",
    params(
        ("root_b64url" = String, Path, description = "Base64url-encoded org_root (git repo) path"),
        ("path" = String, Path, description = "org_root-relative file path"),
        ("v" = Option<String>, Query, description = "Cache-bust version (ignored by server)"),
        ("tree" = Option<String>, Query, description = "Tree OID to read the blob from instead of the origin/main baseline")
    ),
    responses(
        (status = 200, description = "Baseline blob streamed with detected Content-Type"),
        (status = 403, description = "Path is outside allowed context directories"),
        (status = 404, description = "File not in the baseline")
    )
)]
pub async fn serve_baseline_file(
    State(state): State<AppState>,
    Path((root_b64url, rel_path)): Path<(String, String)>,
    Query(query): Query<ViewerQuery>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    serve_baseline_file_request(
        &state.app_handle,
        &root_b64url,
        &rel_path,
        query,
        &headers,
        &state.app_origins,
    )
    .await
}

// Shared byte-serving seam; context allowlisting stays at the app boundary.
async fn serve_baseline_content(
    root_b64url: &str,
    rel_path: &str,
    query: ViewerQuery,
    frame: &FrameRecord,
    app_origins: &crate::http::app_origins::AppOrigins,
) -> Result<Response, StatusCode> {
    let path = resolve_viewer_path(root_b64url, rel_path)?;
    // A deleted working-tree file can still have a legitimate baseline.
    let mut existing = path.as_path();
    while !existing.exists() {
        existing = existing.parent().ok_or(StatusCode::FORBIDDEN)?;
    }
    if !frame.allows_path(existing) {
        return Err(StatusCode::FORBIDDEN);
    }
    let repo_path = PathBuf::from(decode_viewer_root(&root_b64url)?);

    let rel_owned = rel_path.to_owned();
    // SW-GS-10 — a `tree` OID reads that exact tree's blob (conflict redline);
    // absent ⇒ the origin/main publish baseline (SW-GS-3 image/HTML toggle).
    let tree_oid = query.tree.clone();
    let bytes = tokio::task::spawn_blocking(move || match tree_oid {
        Some(oid) => read_tree_blob(&repo_path, &oid, &rel_owned),
        None => read_baseline_blob(&repo_path, &rel_owned),
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Some(bytes) = bytes else {
        return Err(StatusCode::NOT_FOUND);
    };

    let mime = mime_guess::from_path(&rel_path).first_or_octet_stream();
    let content_type = mime.to_string();
    let is_html = mime.type_() == "text" && mime.subtype() == "html";

    let mut response = if is_html {
        let mut body = format!(
            "<script>{LINK_INTERCEPTOR_JS}{DATA_READS_JS}{LIVE_HUB_JS}{VERB_METHODS_JS}</script>"
        )
        .into_bytes();
        body.extend_from_slice(&bytes);
        html_doc_response(content_type, body)
    } else {
        ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
    };
    // Comparisons stay opaque and unbridged even when embedded without the
    // host's baseline sandbox attribute.
    // Apply to every baseline MIME type, including executable SVG documents.
    let csp = format!("sandbox allow-scripts; {}", app_origins.frame_ancestors());
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&csp).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    Ok(response)
}

/// Injected into an inbox message doc only: reports the doc's laid-out height
/// (`codesense:content-size {height}`) so the host can size the inline frame to
/// its content. Re-reports on every resize of the root element.
const MESSAGE_SIZE_JS: &str = r#"(function(){var last=-1;function send(){var d=document.documentElement;if(!d)return;var h=Math.ceil(d.getBoundingClientRect().height);if(h!==last){last=h;window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:content-size',height:h},'*')}}if(typeof ResizeObserver==='function'){new ResizeObserver(send).observe(document.documentElement)}document.addEventListener('DOMContentLoaded',send);window.addEventListener('load',send)})();"#;

/// The body of an inbox message that renders as a live doc: only an `html`
/// message is one. Markdown and plain messages are rendered by the app itself.
fn live_message_html(message: &crate::weft::inbox::InboxMessage) -> Option<&str> {
    (message.content_type == Some(crate::weft::inbox::MessageContentType::Html))
        .then_some(message.text.as_str())
}

/// Serve an `html` inbox message as a live doc, exactly as a dashboard file is
/// served: the same shim, the same proxy handle and read capability (bound to
/// the message's team content root), at the same origin. `org` and `team` are
/// base64url (no padding) and name the inbox holding message `id`. A message
/// that is missing, or is not `html`, is a 404.
#[utoipa::path(
    get,
    path = "/message/{org_b64url}/{team_b64url}/{id}",
    tag = "viewer",
    params(
        ("org_b64url" = String, Path, description = "Base64url-encoded org slug"),
        ("team_b64url" = String, Path, description = "Base64url-encoded team name"),
        ("id" = i64, Path, description = "Inbox message id"),
        ("v" = Option<String>, Query, description = "Cache-bust version (ignored by server)")
    ),
    responses(
        (status = 200, description = "The message rendered as a live HTML doc"),
        (status = 404, description = "No html message with that id in that team's inbox")
    )
)]
pub async fn serve_message(
    State(state): State<AppState>,
    Path((org_b64url, team_b64url, id)): Path<(String, String, i64)>,
    Query(_query): Query<ViewerQuery>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let frame = crate::http::frame_boundary::request_frame(&state.app_handle, &headers)
        .ok_or(StatusCode::FORBIDDEN)?;
    let message = frame.message.as_ref().ok_or(StatusCode::FORBIDDEN)?;
    if id != message.message_id
        || decode_viewer_root(&org_b64url)? != message.org
        || decode_viewer_root(&team_b64url)? != message.team
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let (_, stored) = load_frame_message(&state.app_handle, message)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let html = live_message_html(&stored).ok_or(StatusCode::NOT_FOUND)?;
    Ok(html_doc_response(
        "text/html; charset=utf-8".into(),
        prepend_initial_theme(
            frame_html_body(
                &frame,
                html.as_bytes(),
                &format!(
                    "{MESSAGE_SIZE_JS}{}",
                    frame_first_paint(&state.app_handle, &frame, html.as_bytes())
                ),
            ),
            _query.theme.as_deref(),
            true,
        ),
    ))
}

pub(crate) async fn frame_message_store_path<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    message: &FrameMessage,
) -> Result<(String, PathBuf), String> {
    use crate::commands::dashboard_launch::LaunchTeam;
    use crate::commands::route_to_context::{derive_context_for, ContextResolution};
    let team = LaunchTeam {
        org: message.org.clone(),
        team: message.team.clone(),
    };
    let ContextResolution::Ok(ctx) = derive_context_for(app, &team.org, &team.team).await? else {
        return Err("Message team unavailable".into());
    };
    let content_root = ctx
        .team_content_dir()
        .ok_or("Message content unavailable")?
        .to_string_lossy()
        .into_owned();
    let store_path =
        crate::commands::agent_inbox::team_store_path(ContextResolution::Ok(ctx), &team)?;
    if !store_path.is_file() {
        return Err("Message store unavailable".into());
    }
    Ok((content_root, store_path))
}

pub(crate) async fn load_frame_message<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    message: &FrameMessage,
) -> Result<(String, crate::weft::inbox::InboxMessage), String> {
    let (content_root, store_path) = frame_message_store_path(app, message).await?;
    let id = message.message_id;
    let stored = crate::commands::async_helpers::await_blocking("frame_message", move || {
        // RI-2-ALLOW: the inbox database is read off the async runtime.
        let store = crate::weft::WeftStore::open(store_path).map_err(|e| e.to_string())?;
        crate::weft::inbox::inbox_get(&store, id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Message unavailable".into())
    })
    .await?;
    if live_message_html(&stored).is_none() {
        return Err("Message is not HTML".into());
    }
    Ok((content_root, stored))
}

const VIEWER_ROUTE: &str = "/viewer/{root_b64url}/{*path}";
const PANE_ROUTE: &str = "/pane/{content_b64url}/{panes_b64url}/{*path}";
const BASELINE_ROUTE: &str = "/viewer-baseline/{root_b64url}/{*path}";
const MESSAGE_ROUTE: &str = "/message/{org_b64url}/{team_b64url}/{id}";

/// Let the app itself read a viewer response from script. The iframe's
/// navigation needs no CORS, but the host's `fetch` pre-check of the same URL
/// (deleted file → placeholder, a failing status → the pane's problem banner)
/// is cross-origin from the webview: without `Access-Control-Allow-Origin`
/// WebKit rejects it as a network error ("Load failed"), even though the
/// iframe renders fine. Only the app's own origins are allowed — a doc's body
/// carries the read capability minted for it.
async fn allow_app_origin(
    State(origins): State<crate::http::app_origins::AppOrigins>,
    request: Request,
    next: Next,
) -> Response {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .filter(|origin| {
            origin
                .to_str()
                .is_ok_and(|origin| origins.is_app_origin(origin))
        })
        .cloned();
    let mut response = next.run(request).await;
    if let Some(origin) = origin {
        let headers = response.headers_mut();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.append(header::VARY, HeaderValue::from_static("origin"));
    }
    response
}

/// Apply the app-origin CORS policy to every viewer route, including error
/// statuses, so a pre-check sees a real 404/500 instead of a network error.
fn with_app_cors<S: Clone + Send + Sync + 'static>(
    router: Router<S>,
    origins: crate::http::app_origins::AppOrigins,
) -> Router<S> {
    router.layer(middleware::from_fn_with_state(origins, allow_app_origin))
}

pub fn router(origins: crate::http::app_origins::AppOrigins) -> Router<AppState> {
    with_app_cors(
        Router::new()
            .route(VIEWER_ROUTE, get(serve_file))
            .route(PANE_ROUTE, get(serve_pane))
            .route(BASELINE_ROUTE, get(serve_baseline_file))
            .route(MESSAGE_ROUTE, get(serve_message)),
        origins,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard_stream::frame::FrameGrant;

    #[test]
    fn initial_theme_response_bytes_match_browser_fixtures() {
        let dark = r##"{"scheme":"dark","tokens":{"--cs-bg":"#0d1117","--cs-fg":"#f0f6fc"}}"##;
        let light = r##"{"scheme":"light","tokens":{"--cs-bg":"#ffffff","--cs-fg":"#1f2328"}}"##;
        let centre = b"<!doctype html><html><head></head><body>bare centre document</body></html>";
        let message = b"<!doctype html><html><head></head><body>hosted message child</body></html>";

        assert_eq!(
            prepend_initial_theme(centre.to_vec(), Some(dark), false),
            include_bytes!(
                "../../../../tests/pane-harness/fixtures/startup-paint/centre-dark.html"
            )
        );
        assert_eq!(
            prepend_initial_theme(centre.to_vec(), Some(light), false),
            include_bytes!(
                "../../../../tests/pane-harness/fixtures/startup-paint/centre-light.html"
            )
        );
        assert_eq!(
            prepend_initial_theme(message.to_vec(), Some(dark), true),
            include_bytes!(
                "../../../../tests/pane-harness/fixtures/startup-paint/message-dark.html"
            )
        );
    }

    #[tokio::test]
    async fn initial_theme_precedes_the_bridge_kit_and_authored_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pane.html");
        std::fs::write(&path, "<html><body>pane</body></html>").unwrap();
        let frames = crate::dashboard_stream::frame::FrameStore::default();
        let frame = frames.mint(
            9321,
            FrameGrant {
                frame_id: String::new(),
                principal: "human".into(),
                dashboard: "pane".into(),
                slot: "dock".into(),
                team: None,
                scope: "private".into(),
            },
            dir.path().to_string_lossy().into(),
            path.to_string_lossy().into(),
            dir.path().to_string_lossy().into(),
            crate::dashboard_stream::frame::FramePolicy::default(),
            None,
        );
        let theme = r##"{"scheme":"dark","tokens":{"--cs-bg":"#2e3440","--cs-fg":"#eceff4","--cs-surface":"#3b4252"}}"##;
        let response = serve_frame_file(
            &path,
            &frame,
            &HeaderMap::new(),
            Some(theme),
            |_| String::new(),
            |_| Ok(()),
        )
        .await
        .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let rendered = std::str::from_utf8(&bytes).unwrap();
        assert!(rendered.starts_with("<style id=\"codesense-initial-theme\">"));
        assert!(rendered.contains("color-scheme:var(--cs-color-scheme,dark)"));
        assert!(rendered.contains("--cs-bg:#2e3440"));
        assert!(rendered.find("</style>").unwrap() < rendered.find("<script>").unwrap());
        assert!(rendered.find("</style>").unwrap() < rendered.find("/kit/v1/").unwrap());
        assert!(rendered.contains("background:var(--cs-bg)"));
        let mut center = frame.clone();
        center.grant.slot = "viewer".into();
        let response = serve_frame_file(
            &path,
            &center,
            &HeaderMap::new(),
            Some(theme),
            |_| String::new(),
            |_| Ok(()),
        )
        .await
        .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let rendered = std::str::from_utf8(&bytes).unwrap();
        let css = rendered.split("</style>").next().unwrap();
        assert!(css.contains("--cs-bg:#2e3440"));
        assert!(css.contains("color-scheme:var(--cs-color-scheme,dark)"));
        assert!(!css.contains("html,body"));
        assert!(LINK_INTERCEPTOR_JS.contains(
            "clone.querySelectorAll('#codesense-initial-theme').forEach(function(s){s.remove()})"
        ));
    }

    #[test]
    fn a_context_only_frame_has_no_sessions_first_paint() {
        let frames = crate::dashboard_stream::frame::FrameStore::default();
        let mut frame = frames.mint(
            9321,
            FrameGrant {
                frame_id: String::new(),
                principal: "human".into(),
                dashboard: "board".into(),
                slot: "right-pane".into(),
                team: None,
                scope: "private".into(),
            },
            "/ctx".into(),
            "/ctx/board.html".into(),
            "/ctx".into(),
            crate::dashboard_stream::frame::FramePolicy::default(),
            None,
        );
        frame.effective_grant = vec!["context:read".into()];
        let snapshot = crate::live_hub::projection::Snapshot {
            topic: "sessions".into(),
            view_key: "all".into(),
            rev: 1,
            snapshot: vec![serde_json::json!({"id":"secret-session"})],
            total: None,
            cursor: None,
        };
        assert!(first_paint_script(&frame, &snapshot).is_empty());
        frame.effective_grant = crate::verbs::capabilities::full();
        assert!(first_paint_script(&frame, &snapshot).contains("secret-session"));
    }

    #[test]
    fn first_paint_carries_the_declared_topic_with_the_slot_default() {
        let frames = crate::dashboard_stream::frame::FrameStore::default();
        let mint = |slot: &str| {
            frames.mint(
                9321,
                FrameGrant {
                    frame_id: String::new(),
                    principal: "human".into(),
                    dashboard: "board".into(),
                    slot: slot.into(),
                    team: None,
                    scope: "open".into(),
                },
                "/ctx".into(),
                "/ctx/board.html".into(),
                "/ctx".into(),
                crate::dashboard_stream::frame::FramePolicy::default(),
                None,
            )
        };
        let dock = mint("dock");
        let pane = mint("right-pane");
        assert_eq!(first_paint_topic(&dock, b"<title>A</title>"), "agentList");
        assert_eq!(first_paint_topic(&pane, b"<title>A</title>"), "sessions");
        let declared = br#"<meta name="codesense:first-paint" content="inbox">"#;
        assert_eq!(first_paint_topic(&dock, declared), "inbox");
        assert_eq!(first_paint_topic(&pane, declared), "inbox");
        // The declared topic is still checked against the frame's subscribe reach.
        let mut frame = dock;
        frame.effective_grant = vec!["context:read".into()];
        let snapshot = crate::live_hub::projection::Snapshot {
            topic: "agentList".into(),
            view_key: "all".into(),
            rev: 1,
            snapshot: vec![serde_json::json!({"id":"agent-row","handle":"secret"})],
            total: None,
            cursor: None,
        };
        assert!(first_paint_script(&frame, &snapshot).is_empty());
        frame.effective_grant = crate::verbs::capabilities::full();
        assert!(first_paint_script(&frame, &snapshot).contains("agent-row"));
    }

    #[test]
    fn shim_holds_calls_through_a_rebind_and_reads_its_token_live() {
        assert!(LINK_INTERCEPTOR_JS.contains("type:'codesense:rebind-ack'"));
        assert!(LINK_INTERCEPTOR_JS.contains("if(_rb)_rbQ.push(send);else send()"));
        assert!(LINK_INTERCEPTOR_JS.contains("window.__codesenseProxy.token=d.token"));
        assert!(FRAME_ENV_JS.contains("headers.set('x-codesense-proxy-token',token())"));
    }

    #[test]
    fn frozen_install_and_its_assets_use_the_existing_pane_resolver() {
        let home = tempfile::tempdir().unwrap();
        let source_dir = home.path().join("team/panes");
        std::fs::create_dir_all(source_dir.join("lib")).unwrap();
        std::fs::write(source_dir.join("inbox.html"), "<title>Inbox</title>").unwrap();
        std::fs::write(source_dir.join("lib/widget.js"), "widget").unwrap();
        let source = crate::panes::list_layer(
            &source_dir,
            crate::panes::PaneScope::Team,
            "acme",
            Some("dev"),
        )
        .remove(0);
        let principal = crate::domain::Principal::from(42u64);
        crate::panes::install::choose(home.path(), &principal, &source, true, true).unwrap();
        let space =
            crate::context_repo::scratch_space::org_space(home.path(), &principal, "acme").unwrap();
        let copy = crate::panes::list_layer(
            &space.panes_dir(),
            crate::panes::PaneScope::Private,
            "acme",
            None,
        )
        .remove(0);
        let root = URL_SAFE_NO_PAD.encode(&copy.panes_dir);
        assert_eq!(
            std::fs::read_to_string(resolve_pane_path(&root, "inbox.html").unwrap()).unwrap(),
            "<title>Inbox</title>"
        );
        assert_eq!(
            std::fs::read_to_string(resolve_pane_path(&root, "lib/widget.js").unwrap()).unwrap(),
            "widget"
        );
        assert_eq!(
            resolve_pane_path(&root, "../outside").unwrap_err(),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn first_paint_view_follows_grant_scope_not_document_or_frame() {
        use crate::dashboard_stream::frame::FrameTeam;
        let mut grant = FrameGrant {
            frame_id: "one".into(),
            principal: "human".into(),
            dashboard: "/board.html".into(),
            slot: "centre".into(),
            team: None,
            scope: "private".into(),
        };
        assert_eq!(
            crate::live_hub::view_key::resolve(&grant).as_deref(),
            Some("all")
        );
        grant.team = Some(FrameTeam {
            org: "org".into(),
            team: "team".into(),
        });
        grant.scope = "open".into();
        assert_eq!(
            crate::live_hub::view_key::resolve(&grant).as_deref(),
            Some("all")
        );
        grant.scope = "team".into();
        assert_eq!(
            crate::live_hub::view_key::resolve(&grant).as_deref(),
            Some("team:org/team")
        );
        grant.frame_id = "two".into();
        grant.dashboard = "/other.html".into();
        grant.slot = "message".into();
        assert_eq!(
            crate::live_hub::view_key::resolve(&grant).as_deref(),
            Some("team:org/team")
        );
    }

    // ── app-origin CORS ────────────────────────────────────────────────

    /// The pane route under the viewer's CORS layer, with a stub handler in
    /// place of `serve_pane` (which needs a live `AppHandle`): a 404 for
    /// `gone.html`, else an HTML body.
    fn pane_router_stub() -> Router {
        pane_router_stub_with_origins(crate::http::app_origins::AppOrigins::default())
    }

    fn pane_router_stub_with_origins(origins: crate::http::app_origins::AppOrigins) -> Router {
        async fn stub(Path((_, _, rel)): Path<(String, String, String)>) -> Response {
            if rel == "gone.html" {
                return StatusCode::NOT_FOUND.into_response();
            }
            html_doc_response("text/html".to_string(), b"<p>pane</p>".to_vec())
        }
        with_app_cors(Router::new().route(PANE_ROUTE, get(stub)), origins)
    }

    #[tokio::test]
    #[cfg(debug_assertions)]
    async fn configured_dev_origin_gets_readable_head_success_and_errors() {
        use tower::ServiceExt;
        let config: tauri::Config = serde_json::from_value(serde_json::json!({
            "identifier": "ai.codesense",
            "build": { "devUrl": "http://localhost:5328" }
        }))
        .unwrap();
        let origins = crate::http::app_origins::AppOrigins::from_dev_url(
            config.build.dev_url.as_ref().map(|url| url.as_str()),
        );
        let app = pane_router_stub_with_origins(origins);
        for (file, status) in [
            ("board.html", StatusCode::OK),
            ("gone.html", StatusCode::NOT_FOUND),
        ] {
            for origin in [
                "http://localhost:5328",
                "http://localhost:1420",
                "http://localhost:5329",
            ] {
                let response = app
                    .clone()
                    .oneshot(
                        axum::http::Request::builder()
                            .method("HEAD")
                            .uri(format!("/pane/content/panes/{file}"))
                            .header(header::ORIGIN, origin)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), status);
                assert_eq!(
                    response
                        .headers()
                        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                        .map(|v| v.to_str().unwrap()),
                    (origin == "http://localhost:5328").then_some(origin)
                );
            }
        }
    }

    async fn pane_request(
        method: &str,
        file: &str,
        origin: Option<&str>,
    ) -> axum::response::Response {
        use tower::ServiceExt;
        let content = URL_SAFE_NO_PAD.encode("/home/u/orgs/acme/api/default/content");
        let panes = URL_SAFE_NO_PAD.encode("/home/u/orgs/acme/api/default/panes");
        let mut req = axum::http::Request::builder()
            .method(method)
            .uri(format!("/pane/{content}/{panes}/{file}?v=0"));
        if let Some(origin) = origin {
            req = req.header(header::ORIGIN, origin);
        }
        pane_router_stub()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_pane_route_answers_the_apps_head_precheck_readably() {
        for origin in ["tauri://localhost", "http://tauri.localhost"] {
            let resp = pane_request("HEAD", "board.html", Some(origin)).await;
            assert_eq!(resp.status(), StatusCode::OK, "{origin}");
            assert_eq!(
                resp.headers()
                    .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .unwrap(),
                origin
            );
            assert_eq!(resp.headers().get(header::VARY).unwrap(), "origin");
            let body = http_body_util::BodyExt::collect(resp.into_body())
                .await
                .unwrap()
                .to_bytes();
            assert!(body.is_empty(), "HEAD carries no body");
        }
    }

    #[tokio::test]
    async fn a_missing_pane_is_a_readable_404_not_a_network_error() {
        let resp = pane_request("HEAD", "gone.html", Some("tauri://localhost")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "tauri://localhost"
        );
    }

    #[tokio::test]
    async fn only_the_apps_own_origins_may_read_a_viewer_response() {
        for origin in [Some("https://evil.example"), Some("null"), None] {
            let resp = pane_request("GET", "board.html", origin).await;
            assert_eq!(resp.status(), StatusCode::OK);
            assert!(
                resp.headers()
                    .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .is_none(),
                "{origin:?}"
            );
        }
    }

    // ── resolve_viewer_path ────────────────────────────────────────────

    #[test]
    fn shim_forwards_doc_errors_and_rejections_to_the_parent() {
        assert!(LINK_INTERCEPTOR_JS.contains("codesense:doc-error"));
        assert!(LINK_INTERCEPTOR_JS.contains("addEventListener('error'"));
        assert!(LINK_INTERCEPTOR_JS.contains("addEventListener('unhandledrejection'"));
        // A failed asset is named as the doc wrote it (`lib/chart.js`), not by
        // its resolved URL, whose encoded route segments would bury the path.
        assert!(LINK_INTERCEPTOR_JS.contains("'failed to load '+(t.getAttribute('src')"));
    }

    #[cfg(unix)]
    #[test]
    fn a_pane_link_may_stay_in_panes_but_never_reach_the_rest_of_the_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let panes = tmp.path().join("team/panes");
        std::fs::create_dir_all(panes.join("lib")).unwrap();
        std::fs::write(panes.join("lib/chart.js"), "x").unwrap();
        let secret = tmp.path().join("id_rsa");
        std::fs::write(&secret, "key").unwrap();
        std::os::unix::fs::symlink(panes.join("lib/chart.js"), panes.join("chart.js")).unwrap();
        std::os::unix::fs::symlink(&secret, panes.join("k.txt")).unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app builds");
        let allowed = |name: &str| {
            let file = panes.join(name);
            let real = file.canonicalize().unwrap();
            link_target_allowed(&file, &real, app.handle())
        };
        assert!(allowed("chart.js"), "a link within panes/ is served");
        assert!(
            !allowed("k.txt"),
            "a link out of the context dirs is refused"
        );
    }

    #[test]
    fn pane_paths_stay_inside_their_panes_dir() {
        let panes = URL_SAFE_NO_PAD.encode("/home/u/orgs/acme/api/default/panes");
        assert_eq!(
            resolve_pane_path(&panes, "board.html").unwrap(),
            PathBuf::from("/home/u/orgs/acme/api/default/panes/board.html")
        );
        assert_eq!(
            resolve_pane_path(&panes, "lib/chart.js").unwrap(),
            PathBuf::from("/home/u/orgs/acme/api/default/panes/lib/chart.js")
        );
        for escape in ["../content/secret.md", "lib/../../config/x.yml", ""] {
            assert_eq!(
                resolve_pane_path(&panes, escape).unwrap_err(),
                StatusCode::FORBIDDEN,
                "{escape}"
            );
        }
        let not_panes = URL_SAFE_NO_PAD.encode("/home/u/orgs/acme/api/default/content");
        assert_eq!(
            resolve_pane_path(&not_panes, "board.html").unwrap_err(),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn resolves_simple_path() {
        let root = URL_SAFE_NO_PAD.encode("/home/user/repo");
        let path = resolve_viewer_path(&root, "docs/index.html").unwrap();
        assert_eq!(path, PathBuf::from("/home/user/repo/docs/index.html"));
    }

    #[test]
    fn normalizes_dotdot_components() {
        let root = URL_SAFE_NO_PAD.encode("/home/user/repo");
        let path = resolve_viewer_path(&root, "docs/../secret.txt").unwrap();
        assert_eq!(path, PathBuf::from("/home/user/repo/secret.txt"));
    }

    #[test]
    fn traversal_escapes_root() {
        let root = URL_SAFE_NO_PAD.encode("/home/user/repo");
        let path = resolve_viewer_path(&root, "../../etc/passwd").unwrap();
        assert_eq!(path, PathBuf::from("/home/etc/passwd"));
    }

    #[test]
    fn rejects_invalid_base64() {
        let result = resolve_viewer_path("not-valid-b64!!!", "file.html");
        assert_eq!(result, Err(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn decode_viewer_root_roundtrips_and_rejects_garbage() {
        let root = URL_SAFE_NO_PAD.encode("/home/user/repo/content");
        assert_eq!(
            decode_viewer_root(&root).unwrap(),
            "/home/user/repo/content"
        );
        assert_eq!(
            decode_viewer_root("not-valid-b64!!!"),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    // ── MIME detection ─────────────────────────────────────────────────

    #[test]
    fn mime_html() {
        let mime = mime_guess::from_path("test.html").first_or_octet_stream();
        assert_eq!(mime.type_().as_str(), "text");
        assert_eq!(mime.subtype().as_str(), "html");
    }

    #[test]
    fn mime_png() {
        let mime = mime_guess::from_path("image.png").first_or_octet_stream();
        assert_eq!(mime.to_string(), "image/png");
    }

    // SW-SC-7: confirm the image extensions added to the viewer need NO MIME
    // mapping change — mime_guess already resolves them. `<img>`-loaded SVG is
    // rendered as an asset (no script execution), satisfying AC-3/AC-4.
    #[test]
    fn mime_svg() {
        let mime = mime_guess::from_path("diagram.svg").first_or_octet_stream();
        assert_eq!(mime.to_string(), "image/svg+xml");
    }

    #[test]
    fn mime_webp() {
        let mime = mime_guess::from_path("shot.webp").first_or_octet_stream();
        assert_eq!(mime.to_string(), "image/webp");
    }

    #[test]
    fn mime_jpeg() {
        let mime = mime_guess::from_path("photo.jpg").first_or_octet_stream();
        assert_eq!(mime.to_string(), "image/jpeg");
    }

    #[test]
    fn mime_unknown_extension() {
        let mime = mime_guess::from_path("data.qqq123").first_or_octet_stream();
        assert_eq!(mime.to_string(), "application/octet-stream");
    }

    #[test]
    fn mime_css() {
        let mime = mime_guess::from_path("style.css").first_or_octet_stream();
        assert_eq!(mime.to_string(), "text/css");
    }

    #[test]
    fn mime_javascript() {
        let mime = mime_guess::from_path("app.js").first_or_octet_stream();
        assert!(mime.to_string().contains("javascript"));
    }

    // ── Script injection ──────────────────────────────────────────────

    #[test]
    fn html_mime_detected_for_html_extension() {
        let mime = mime_guess::from_path("page.html").first_or_octet_stream();
        assert_eq!(mime.type_().as_str(), "text");
        assert_eq!(mime.subtype().as_str(), "html");
    }

    #[test]
    fn html_mime_detected_for_htm_extension() {
        let mime = mime_guess::from_path("page.htm").first_or_octet_stream();
        assert_eq!(mime.type_().as_str(), "text");
        assert_eq!(mime.subtype().as_str(), "html");
    }

    #[test]
    fn interceptor_script_is_non_empty() {
        assert!(!LINK_INTERCEPTOR_JS.is_empty());
        assert!(LINK_INTERCEPTOR_JS.contains("codesense:navigate"));
        assert!(LINK_INTERCEPTOR_JS.contains("codesense:external-link"));
    }

    // SW-LD-1: the injected script also exposes the window.codesense bridge shim
    // (postMessage RPC). Assert its presence + the request/result message types.
    // The parent enforces the allowlist; this shim holds no policy.
    #[test]
    fn interceptor_script_has_codesense_bridge() {
        assert!(LINK_INTERCEPTOR_JS.contains("window.codesense"));
        assert!(LINK_INTERCEPTOR_JS.contains("codesense:invoke"));
        assert!(LINK_INTERCEPTOR_JS.contains("codesense:invoke-result"));
        // Bridge replies are gated on the parent as the message source (same
        // guard as the scroll-restore handler).
        assert!(LINK_INTERCEPTOR_JS.contains("e.source!==window.parent"));
    }

    // SW-SD-1: the shim gains the snapshot channel — a parent→doc
    // `codesense:snapshot-request` responder that serializes the frozen DOM,
    // and a doc-facing `window.codesense.snapshot(state?)` verb. Assert the
    // pieces are present, that ALL scripts are stripped, that the state blob is
    // the inert tagged JSON element, and that the injection-safe `<`→`<`
    // escape is applied (a `</script>` in a value can't break the raw-text
    // element — the doc-side twin of `proxy_script_json_encoding_is_injection_safe`).
    #[test]
    fn interceptor_script_has_snapshot() {
        // Doc-facing verb + the two message types (request in, result back).
        assert!(
            LINK_INTERCEPTOR_JS.contains("window.codesense.snapshot")
                || LINK_INTERCEPTOR_JS.contains("snapshot:function")
        );
        assert!(LINK_INTERCEPTOR_JS.contains("codesense:snapshot-request"));
        assert!(LINK_INTERCEPTOR_JS.contains("codesense:snapshot-result"));
        // Doc-initiated post to the parent for writing — pin the exact shape so
        // this isn't trivially satisfied by the longer `-request`/`-result` names
        // (review r1).
        assert!(LINK_INTERCEPTOR_JS.contains("type:'codesense:snapshot',id:id,html:h"));
        // Serialization: clone the document, strip EVERY script (offline-safe) —
        // INCLUDING scripts nested inside <template> content, which
        // `querySelectorAll('script')` does not descend into (review r1).
        assert!(LINK_INTERCEPTOR_JS.contains("cloneNode"));
        assert!(LINK_INTERCEPTOR_JS.contains("querySelectorAll('script')"));
        assert!(LINK_INTERCEPTOR_JS.contains("querySelectorAll('template')"));
        // Inert, machine-readable state sidecar (never executed).
        assert!(LINK_INTERCEPTOR_JS.contains("application/json"));
        assert!(LINK_INTERCEPTOR_JS.contains("codesense-snapshot-state"));
        // Injection-safe: the JSON is escaped so a `</script>` can't break out
        // of the raw-text <script> element (mirrors render_proxy_script:169).
        assert!(LINK_INTERCEPTOR_JS.contains(r"replace(/</g,'\\u003c')"));
        // Still gated on the parent as the message source (AC-7 discipline).
        assert!(LINK_INTERCEPTOR_JS.contains("e.source!==window.parent"));
    }

    // SW-DX-4 / SW-OC-26: the shim's read-only SDK — `version`, `firstPaint`,
    // `sprints()`, `taxonomy()` over the per-render `readCap`, and the context
    // engine via the bridge (`queryContext` / `subscribeContext`). The retired
    // HTTP `query()` and SSE `onStatus()` are gone.
    #[test]
    fn verb_methods_come_after_the_live_hub_so_hand_written_methods_win() {
        let body = String::from_utf8(live_doc_body(String::new(), b"<p>x</p>", "")).unwrap();
        let hub = body.find(LIVE_HUB_JS).unwrap();
        assert!(body.find(VERB_METHODS_JS).unwrap() > hub);
    }

    #[test]
    fn interceptor_script_has_read_sdk() {
        let sdk = LINK_INTERCEPTOR_JS
            .split_once("window.codesense={")
            .expect("the SDK object is declared")
            .1
            .split_once("};window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:ready'")
            .expect("the SDK object ends before its ready message")
            .0;
        assert!(sdk.contains(&format!("version:'{PAGE_SDK_VERSION}'")));
        assert!(LINK_INTERCEPTOR_JS.contains("firstPaint"));
        assert!(LINK_INTERCEPTOR_JS.contains("sprints:function"));
        assert!(LINK_INTERCEPTOR_JS.contains("fetch(px.base+'/api/v1/sprints'"));
        assert!(LINK_INTERCEPTOR_JS.contains("taxonomy:function"));
        assert!(LINK_INTERCEPTOR_JS.contains("fetch(px.base+'/api/v1/taxonomy'"));
        // The read routes use the per-render `readCap`, never the shared token.
        assert!(LINK_INTERCEPTOR_JS.contains("'x-codesense-proxy-token':px.readCap"));
        // Retired: the HTTP query route and the SSE status stream.
        assert!(!LINK_INTERCEPTOR_JS.contains("query:function(filter)"));
        assert!(!LINK_INTERCEPTOR_JS.contains("onStatus:"));
        assert!(!LINK_INTERCEPTOR_JS.contains("/api/v1/query"));
        assert!(!LINK_INTERCEPTOR_JS.contains("EventSource"));
    }

    // SW-DX-4 AC-6/AC-13: degraded reads + honest expiry: no `readCap` rejects
    // and a 401 (expired token) is a distinct typed `expired` rejection.
    #[test]
    fn interceptor_read_sdk_degrades_and_is_honest() {
        assert!(LINK_INTERCEPTOR_JS.contains("if(!px||!px.readCap)return Promise.reject"));
        assert!(LINK_INTERCEPTOR_JS
            .contains("r.status===401)return Promise.reject(new Error('expired')"));
    }

    // SW-DX-6 AC-1: the shim exposes an `act(verb, args)` write verb. It is a
    // thin wrapper that reuses the existing `invoke` bridge —
    // same envelope, same correlation, same origin-pinned settle. It must NOT
    // open a second channel or add its own gate.
    #[test]
    fn interceptor_script_has_act() {
        // The `act` write verb is present on the shim.
        assert!(LINK_INTERCEPTOR_JS.contains("act:function"));
        // It delegates straight through to `invoke` — no separate envelope, no
        // new `codesense:*` message type, no gate of its own.
        assert!(LINK_INTERCEPTOR_JS.contains("act:function(v,a){return this.invoke(v,a)}"));
    }

    // SW-DX-11 (AC-1/AC-13): the shim exposes a `readWorkspace(args)` convenience
    // method — a thin `invoke('readWorkspace', …)` wrapper, exactly like `act`.
    // It opens NO second channel and adds no gate of its own; enforcement
    // (confinement / traversal / managed / audit) is all app-side.
    #[test]
    fn interceptor_script_has_read_workspace() {
        assert!(LINK_INTERCEPTOR_JS.contains("readWorkspace:function"));
        assert!(LINK_INTERCEPTOR_JS
            .contains("readWorkspace:function(a){return this.invoke('readWorkspace',a)}"));
    }

    // SW-OC-26: the shim exposes the context engine as `queryContext(q)` (a bridge
    // read verb) and `subscribeContext(q, cb)` (re-runs on the parent's
    // `codesense:context-changed` push, generation-guarded).
    #[test]
    fn shim_exposes_query_context_and_subscribe_context() {
        assert!(LINK_INTERCEPTOR_JS
            .contains("queryContext:function(q){return this.invoke('queryContext',q||{})}"));
        assert!(LINK_INTERCEPTOR_JS.contains("subscribeContext:function(q,cb)"));
        assert!(LINK_INTERCEPTOR_JS.contains("d.type!=='codesense:context-changed'"));
        // Re-runs only when the change touches the result's scope orgs.
        assert!(LINK_INTERCEPTOR_JS.contains("so=(r&&r.scopeOrgs)||null"));
    }

    // SW-DNS-1: the nav selection is opt-in and read-only. The first call to
    // either method asks the host to start pushing; the shim only accepts the push
    // from its parent; nothing about it goes through `invoke` (no allow-list verb).
    // SW-DNS-2: a dashboard's own settings are plain bridge verbs; the host, not
    // the page, decides whose entry they touch.
    // The inbox-changed callback receives the host's message, so a page can re-read just
    // the messages it names.
    #[test]
    fn shim_passes_the_inbox_change_to_its_callback() {
        assert!(LINK_INTERCEPTOR_JS.contains(
            "e.data.type==='codesense:inbox-changed')cb(e.data)}window.addEventListener('message',changed)"
        ));
    }

    #[test]
    fn shim_carries_prefs() {
        assert!(LINK_INTERCEPTOR_JS.contains("prefs:function(){return this.invoke('prefs')}"));
        assert!(LINK_INTERCEPTOR_JS
            .contains("setPrefs:function(p){return this.invoke('setPrefs',p||{})}"));
    }

    #[test]
    fn shim_carries_host_menus_popovers_and_focus_return() {
        for verb in ["showMenu", "showPopover", "focusOut", "focusTerminal"] {
            assert!(LINK_INTERCEPTOR_JS.contains(&format!("this.invoke('{verb}'")));
        }
        assert!(LINK_INTERCEPTOR_JS.contains("onFocusReturn:function(cb)"));
        assert!(LINK_INTERCEPTOR_JS.contains("d.type==='codesense:focus-return'"));
        assert!(LINK_INTERCEPTOR_JS.contains("closest('[data-codesense-row]')"));
    }

    #[test]
    fn shim_aliases_nav_selection_to_slot_context() {
        assert!(LINK_INTERCEPTOR_JS.contains("getNavSelection:function()"));
        assert!(LINK_INTERCEPTOR_JS.contains("onNavSelectionChange:function(cb)"));
        assert!(LINK_INTERCEPTOR_JS.contains("d.type==='codesense:slot-context'"));
        // A read without a host still settles; listeners use slot context alone.
        assert!(!LINK_INTERCEPTOR_JS.contains("codesense:nav-selection-watch"));
        assert!(LINK_INTERCEPTOR_JS.contains("res(null)},5000)"));
        assert!(!LINK_INTERCEPTOR_JS.contains("invoke('getNavSelection'"));
        assert!(!LINK_INTERCEPTOR_JS.contains("invoke('navSelection'"));
    }

    // A pane is first-class chrome: the shim asks the host for its theme and
    // visibility once it runs, applies the theme tokens as CSS custom properties
    // plus `color-scheme`, and carries the roster, agents and badge channels.
    #[test]
    fn shim_carries_theme_visibility_roster_agents_and_badge() {
        assert!(
            LINK_INTERCEPTOR_JS.contains("window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:ready',sentAt:_csNow()},'*')")
        );
        assert!(LINK_INTERCEPTOR_JS.contains("d.type==='codesense:theme'"));
        // The host reveals the frame on this ack, after the page has applied the theme.
        assert!(LINK_INTERCEPTOR_JS.contains("(typeof requestAnimationFrame==='function'?requestAnimationFrame:function(f){setTimeout(f,0)})(function(){window.parent.postMessage({token:window.__codesenseProxy&&window.__codesenseProxy.token,type:'codesense:theme-applied'}"));
        assert!(LINK_INTERCEPTOR_JS.contains("r.style.setProperty(n,k[n])"));
        assert!(LINK_INTERCEPTOR_JS.contains("r.style.colorScheme=sc"));
        // Outside a pane the host answers `theme:null`, so `theme()` never hangs,
        // and a snapshot carries the doc's own styling, not the theme of the moment.
        assert!(LINK_INTERCEPTOR_JS
            .contains("_thNone=true;_thWait.splice(0).forEach(function(f){f(null)})"));
        assert!(LINK_INTERCEPTOR_JS
            .contains("_thKeys.forEach(function(n){clone.style.removeProperty(n)})"));
        assert!(LINK_INTERCEPTOR_JS.contains("theme:function()"));
        assert!(LINK_INTERCEPTOR_JS.contains("onTheme:function(cb)"));
        assert!(LINK_INTERCEPTOR_JS.contains("d.type==='codesense:visibility'"));
        // SV-ENG-120: a `hello` is answered with one more `ready` only until the host has answered.
        assert!(LINK_INTERCEPTOR_JS.contains(
            "if(d.type==='codesense:hello'){if(!window.__csSlotSeen)window.parent.postMessage("
        ));
        assert!(LINK_INTERCEPTOR_JS.contains("type:'codesense:ready',retry:true"));
        assert!(LINK_INTERCEPTOR_JS.contains("d.type==='codesense:theme'){window.__csSlotSeen=true;"));
        assert!(LINK_INTERCEPTOR_JS.contains("onVisibility:function(cb)"));
        assert!(LINK_INTERCEPTOR_JS.contains("onIntent:function(cb)"));
        assert!(LINK_INTERCEPTOR_JS.contains("codesense:intent"));
        assert!(LINK_INTERCEPTOR_JS.contains("sessions:function(){return this.invoke('sessions')}"));
        assert!(LIVE_HUB_JS.contains("c.onSessions = function"));
        for method in [
            "agent:function(a)",
            "inbox:function(a)",
            "roles:function(a)",
            "updates:function(a)",
            "humanStatus:function()",
            "onAgentsChanged:function(cb)",
            "onInboxChanged:function(cb)",
        ] {
            assert!(LINK_INTERCEPTOR_JS.contains(method), "shim lacks {method}");
        }
        assert!(LINK_INTERCEPTOR_JS.contains("'codesense:agents-changed'"));
        assert!(LIVE_HUB_JS.contains("codesense:delta"));
        assert!(LINK_INTERCEPTOR_JS.contains("agents:function(){return this.invoke('agents')}"));
        assert!(LINK_INTERCEPTOR_JS.contains("setBadge:function(b)"));
        assert!(LINK_INTERCEPTOR_JS.contains("type:'codesense:badge'"));
        // Where the page lives, opening a file and writing a frontmatter field are
        // bridge verbs: the host resolves and validates them, never the page.
        assert!(LINK_INTERCEPTOR_JS.contains("context:function(){return this.invoke('context')}"));
        assert!(LINK_INTERCEPTOR_JS.contains("openFile:function(t,o){"));
        assert!(
            LINK_INTERCEPTOR_JS.contains("this.invoke('openFile',{path:a.path,team:a.team,newTab:")
        );
        assert!(
            LINK_INTERCEPTOR_JS.contains("setField:function(a){return this.invoke('setField',a)}")
        );
    }

    // SW-DX-4 AC-5: the embedded first-paint payload is parsed from the inert
    // `codesense-dashboard-data` element, and it SURVIVES an SW-SD-1 capture —
    // `serializeSnapshot` re-bakes it (SW-SD-1's `stripScripts` removes every
    // `<script>`, incl. `application/json`) with the same `<`→escape discipline
    // so an exported dashboard re-hydrates identically offline.
    #[test]
    fn interceptor_firstpaint_reads_and_rebakes_dashboard_data() {
        // firstPaint parses the inert embedded element once (try/catch → null).
        assert!(LINK_INTERCEPTOR_JS.contains("getElementById('codesense-dashboard-data')"));
        // Inert, machine-readable (never executed), distinct id from SW-SD-1's state.
        assert!(LINK_INTERCEPTOR_JS.contains("id='codesense-dashboard-data'"));
        // Capture re-bake: read the live payload BEFORE stripping, re-inject it,
        // and re-escape `<` so an author-written payload can't break out.
        assert!(LINK_INTERCEPTOR_JS.contains("dblob.textContent=_ddText.replace(/</g,'\\\\u003c')"));
    }

    // SW-LD-1: a focused iframe swallows ⌘W, so the injected script forwards it
    // to the parent as `codesense:close-tab` (parity with other file types).
    #[test]
    fn interceptor_script_forwards_close_tab_shortcut() {
        assert!(LINK_INTERCEPTOR_JS.contains("keydown"));
        assert!(LINK_INTERCEPTOR_JS.contains("codesense:close-tab"));
        assert!(LINK_INTERCEPTOR_JS.contains("metaKey"));
    }

    #[test]
    fn interceptor_script_forwards_shortcut_chords() {
        assert!(LINK_INTERCEPTOR_JS.contains("window.addEventListener('keydown'"));
        assert!(LINK_INTERCEPTOR_JS.contains("e.defaultPrevented||e.isComposing"));
        assert!(LINK_INTERCEPTOR_JS
            .contains(r"if(!e.metaKey&&!e.ctrlKey&&!e.altKey&&!/^F\d{1,2}$/.test(e.key))return"));
        assert!(LINK_INTERCEPTOR_JS.contains("codesense:key-escape"));
        assert!(LINK_INTERCEPTOR_JS.contains("type:'codesense:shortcut',key:e.key,code:e.code"));
        assert!(
            LINK_INTERCEPTOR_JS.contains("setTimeout(function(){if(!e.defaultPrevented||hostOnly)")
        );
        // Command-P is an ordinary registry chord now; only Command-W keeps its path.
        assert!(!LINK_INTERCEPTOR_JS.contains("quick-open"));
        assert!(LINK_INTERCEPTOR_JS.contains("codesense:close-tab"));
        // Only the exact reserved Command-W closes; other W chords and modified
        // arrows forward like any chord, and the page keeps them with preventDefault.
        assert!(LINK_INTERCEPTOR_JS.contains(
            "if((e.metaKey||e.ctrlKey)&&!e.shiftKey&&!e.altKey&&(e.key==='w'||e.key==='W'))"
        ));
        assert!(!LINK_INTERCEPTOR_JS.contains("/^Arrow/"));
        assert!(!LINK_INTERCEPTOR_JS.contains("switch-org"));
    }

    #[test]
    fn interceptor_script_registers_and_receives_dashboard_commands() {
        assert!(LINK_INTERCEPTOR_JS.contains(
            "commands:{register:function(ids){return window.codesense.invoke('commands.register',{ids:ids})},list:function(){return window.codesense.invoke('commands.list',{})},run:function(id,args){return window.codesense.invoke('commands.run',{id:id,args:args})}}"
        ));
        assert!(LINK_INTERCEPTOR_JS.contains("onCommand:function(cb){_cmdL.push(cb)"));
        assert!(LINK_INTERCEPTOR_JS.contains(
            "d.type==='codesense:command'){_cmdL.slice().forEach(function(cb){cb({id:d.id,args:d.args,dispatchId:d.dispatchId})})"
        ));
    }

    #[test]
    fn interceptor_script_has_scroll_handlers() {
        // SW-FX-5: scroll reporter posts the current offset to the parent...
        assert!(LINK_INTERCEPTOR_JS.contains("codesense:scroll"));
        assert!(LINK_INTERCEPTOR_JS.contains("window.scrollY"));
        // ...and the restore handler scrolls on a parent-originated message.
        assert!(LINK_INTERCEPTOR_JS.contains("codesense:restore-scroll"));
        assert!(LINK_INTERCEPTOR_JS.contains("window.scrollTo"));
        // AC-4: restore must be gated on the parent as the message source.
        assert!(LINK_INTERCEPTOR_JS.contains("e.source!==window.parent"));
        // Restore rejects non-finite offsets and re-asserts after layout.
        assert!(LINK_INTERCEPTOR_JS.contains("isFinite"));
    }

    /// The data reads ride every injected shim: each method invokes the bridge verb of
    /// its name, and each `on*Changed` listens for the host's nudge.
    #[test]
    fn shim_carries_the_routine_run_worktree_and_machine_reads() {
        for (method, verb) in [
            ("routines:function()", "'routines'"),
            ("routine:function(a)", "'routine',a"),
            ("routineRuns:function(a)", "'routineRuns',a"),
            ("routineRun:function(a)", "'routineRun',a"),
            ("runs:function(a)", "'runs',a||{}"),
            ("run:function(a)", "'run',a"),
            ("runEvidence:function(a)", "'runEvidence',a"),
            ("runWorkProduct:function(a)", "'runWorkProduct',a"),
            ("worktrees:function()", "'worktrees'"),
            ("machines:function()", "'machines'"),
            ("storyChanges:function(a)", "'storyChanges',a"),
            ("whyLine:function(a)", "'whyLine',a"),
            ("setup:function()", "'setup'"),
            ("sessionUsage:function(a)", "'sessionUsage',a||{}"),
            ("modelRates:function()", "'modelRates'"),
            ("canPasteToTerminal:function()", "'canPasteToTerminal'"),
        ] {
            assert!(DATA_READS_JS.contains(method), "shim lacks {method}");
            assert!(
                DATA_READS_JS.contains(&format!("c.invoke({verb})")),
                "{method} invokes {verb}"
            );
        }
        for (on, event) in [
            ("onRoutinesChanged", "codesense:routines-changed"),
            ("onMachinesChanged", "codesense:machines-changed"),
            ("onWorktreesChanged", "codesense:worktrees-changed"),
            ("onPasteTargetChanged", "codesense:paste-target-changed"),
        ] {
            assert!(DATA_READS_JS.contains(&format!("{on}:on('{event}')")));
        }
        assert!(DATA_READS_JS.contains("e.source===window.parent"));
    }

    #[test]
    fn script_injection_produces_correct_prefix() {
        let script_tag = format!("<script>{LINK_INTERCEPTOR_JS}</script>");
        assert!(script_tag.starts_with("<script>"));
        assert!(script_tag.ends_with("</script>"));

        // Simulate what serve_file does for HTML:
        let original_html = b"<!DOCTYPE html><html><body>hello</body></html>";
        let mut body = script_tag.into_bytes();
        body.extend_from_slice(original_html);
        let result = String::from_utf8(body).unwrap();

        assert!(result.starts_with("<script>"));
        assert!(result.contains("codesense:navigate"));
        assert!(result.ends_with("</body></html>"));
    }

    #[test]
    fn non_html_mime_not_injected() {
        // CSS, JS, PNG should not be detected as HTML
        for path in &["style.css", "app.js", "image.png", "data.json"] {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let is_html = mime.type_() == "text" && mime.subtype() == "html";
            assert!(!is_html, "{path} should not be detected as HTML");
        }
    }

    // ── SW-LD-2: proxy capability injection ───────────────────────────────

    #[test]
    fn proxy_script_exposes_base_and_json_encoded_token() {
        let script = render_proxy_script("abc123", "");
        assert!(script.starts_with("<script>window.__codesenseProxy="));
        assert!(script.contains("base:location.origin"));
        // Token is JSON-encoded (quoted), never a bare identifier.
        assert!(script.contains(r#"token:"abc123""#));
        // SW-LD-5 AC-8: an empty summary still emits a `providers` object.
        assert!(script.contains("providers:{}"));
        assert!(script.ends_with("</script>"));
    }

    #[test]
    fn proxy_script_carries_non_secret_provider_summary() {
        // SW-LD-5 AC-8: a connected Atlassian surfaces connected+site (NO token).
        let script = render_proxy_script(
            "sekret",
            r#"{"jira":{"connected":true,"site":"https://acme.atlassian.net"}}"#,
        );
        assert!(script.contains(
            r#"providers:{"jira":{"connected":true,"site":"https://acme.atlassian.net"}}"#
        ));
        // Disconnected form.
        let script = render_proxy_script("sekret", r#"{"jira":{"connected":false}}"#);
        assert!(script.contains(r#"providers:{"jira":{"connected":false}}"#));
    }

    #[test]
    fn both_sdk_lanes_use_the_frame_credential() {
        let script = render_proxy_script("per-frame-token", "");
        assert!(script.contains(r#"token:"per-frame-token""#));
        assert!(script.contains(r#"readCap:"per-frame-token""#));
    }

    #[test]
    fn proxy_script_json_encoding_is_injection_safe() {
        // Secrets are hex today, but an adversarial value must not break out of
        // the <script>: the quote is JSON-escaped and every `<` is neutralized,
        // so no stray `<` (hence no premature `</script>`) survives the payload.
        let script = render_proxy_script("a\"</script>", "");
        let inner = script
            .strip_prefix("<script>")
            .and_then(|s| s.strip_suffix("</script>"))
            .expect("wrapped in a single script element");
        assert!(
            !inner.contains('<'),
            "no stray `<` inside the injected payload"
        );
        // The ONLY literal `</script>` is the closing tag we appended.
        assert_eq!(script.matches("</script>").count(), 1);
    }

    // ── inbox message docs ─────────────────────────────────────────────

    fn message(
        content_type: Option<crate::weft::inbox::MessageContentType>,
    ) -> crate::weft::inbox::InboxMessage {
        crate::weft::inbox::InboxMessage {
            id: 7,
            from: "agent-1".into(),
            to: crate::weft::inbox::HUMAN.into(),
            text: "<script>codesense.sessions()</script>".into(),
            created_at_ms: 0,
            read_at_ms: None,
            in_reply_to: None,
            in_reply_to_team: None,
            primary_file: None,
            attachments: Vec::new(),
            archived_at_ms: None,
            withdrawn_at_ms: None,
            delivery: None,
            on_click: None,
            content_type,
            via: None,
            role: None,
            reply_to: None,
            summary: None,
            kind: None,
        }
    }

    // Only an `html` message is served as a live doc, its body verbatim (scripts
    // included); markdown, plain and untyped messages are the app's to render.
    #[test]
    fn only_html_messages_are_live_docs() {
        use crate::weft::inbox::MessageContentType as C;
        let html = message(Some(C::Html));
        assert_eq!(
            live_message_html(&html),
            Some("<script>codesense.sessions()</script>")
        );
        assert_eq!(live_message_html(&message(Some(C::Markdown))), None);
        assert_eq!(live_message_html(&message(Some(C::Plain))), None);
        assert_eq!(live_message_html(&message(None)), None);
    }

    // The size reporter is a self-contained script with no closing tag of its own.
    #[test]
    fn message_size_script_reports_content_size() {
        assert!(MESSAGE_SIZE_JS.contains("codesense:content-size"));
        assert!(!MESSAGE_SIZE_JS.contains("</script>"));
    }

    // Every live doc gets the kit after the shim it extends, and its own bytes
    // follow unchanged, so a doc that uses no `cs-` class renders as before.
    #[test]
    fn a_live_doc_gets_the_kit_after_the_shim_and_keeps_its_own_bytes() {
        let doc = b"<!doctype html><style>body{color:#333}</style><p>plain report</p>";
        let body = String::from_utf8(live_doc_body(String::new(), doc, "")).unwrap();
        let shim = body.find("window.codesense={").unwrap();
        let css = body.find("/kit/v1/kit.css?v=").unwrap();
        let js = body.find("<script src=\"/kit/v1/kit.js?v=").unwrap();
        let own = body.rfind("<!doctype html>").unwrap();
        assert!(shim < css && css < js && js < own, "{body}");
        assert!(body.ends_with(std::str::from_utf8(doc).unwrap()));
        // It does not style itself with the app's tokens: its root keeps its scheme.
        assert!(!body.contains("window.__codesenseThemeAware=true</script>"));
    }

    #[test]
    fn a_doc_that_uses_the_app_tokens_is_marked_theme_aware() {
        for doc in [
            &b"<style>body{color:var(--cs-fg)}</style>"[..],
            &b"<body class=\"cs-app\">"[..],
        ] {
            let body = String::from_utf8(live_doc_body(String::new(), doc, "")).unwrap();
            let mark = body.find("window.__codesenseThemeAware=true").unwrap();
            assert!(mark < body.find(std::str::from_utf8(doc).unwrap()).unwrap());
        }
    }

    #[test]
    fn a_message_doc_gets_the_kit_and_its_size_reporter() {
        let body =
            String::from_utf8(live_doc_body(String::new(), b"<p>hi</p>", MESSAGE_SIZE_JS)).unwrap();
        assert!(body.contains("/kit/v1/kit.js"));
        assert!(body.contains("codesense:content-size"));
        assert!(body.ends_with("<p>hi</p>"));
    }

    // The server's parse-time stylesheet sets the UA scheme for every themed
    // document. Later bridge updates still preserve a centre doc's authored look.
    #[test]
    fn the_shim_sets_the_root_scheme_only_when_asked_or_theme_aware() {
        assert!(LINK_INTERCEPTOR_JS.contains(
            "_applyTheme(d.theme,d.applyScheme!==false||window.__codesenseThemeAware===true)"
        ));
        assert!(LINK_INTERCEPTOR_JS.contains("r.style.setProperty('--cs-color-scheme',sc)"));
        assert!(LINK_INTERCEPTOR_JS.contains("if(own){r.style.colorScheme=sc;_thOwn=true}"));
        assert!(
            LINK_INTERCEPTOR_JS.contains("if(_thOwn)clone.style.removeProperty('color-scheme')")
        );
    }

    // The kit is served as files and inlined nowhere: neither may close the tag
    // it would sit in were it inlined, and the script must extend the shim.
    #[test]
    fn the_kit_files_are_self_contained() {
        use super::super::dashboard_kit::{KIT_CSS, KIT_JS};
        assert!(!KIT_JS.contains("</script"));
        assert!(!KIT_CSS.contains("</style"));
        assert!(KIT_JS.contains("window.codesense = window.codesense || {}"));
    }
}

#[cfg(test)]
mod frame_security_tests {
    use super::*;
    use tower::ServiceExt;

    fn fixture_router() -> Router {
        crate::http::frame_boundary::with_boundary(
            Router::new().fallback(|| async { "private-bytes" }),
            crate::http::frame_boundary::FrameBoundary {
                app_origins: crate::http::app_origins::AppOrigins::default(),
                frames: std::sync::Arc::new(crate::dashboard_stream::frame::FrameStore::default()),
                port: 9321,
            },
        )
    }

    #[tokio::test]
    async fn unknown_host_cannot_rebind_the_server() {
        let response = fixture_router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .header("host", "attacker.example:9321")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn viewer_requires_a_live_frame_record() {
        let response = fixture_router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/viewer/root/private.html")
                    .header("host", "localhost:9321")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn foreign_origin_cannot_call_local_routes() {
        let response = fixture_router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .header("host", "localhost:9321")
                    .header("origin", "https://attacker.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn restricted_frames_cannot_read_foreign_or_private_baseline_blobs() {
        use crate::dashboard_stream::frame::{FrameGrant, FramePolicy, FrameStore};
        let temp = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(temp.path()).unwrap();
        for folder in ["own", "foreign", "private"] {
            std::fs::create_dir(temp.path().join(folder)).unwrap();
            std::fs::write(temp.path().join(folder).join("board.html"), folder).unwrap();
        }
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        let oid = index.write_tree().unwrap();
        let tree = repo.find_tree(oid).unwrap();
        let signature = git2::Signature::now("test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap();
        let store = FrameStore::default();
        let mut frame = store.mint(
            9321,
            FrameGrant {
                frame_id: String::new(),
                principal: "human".into(),
                dashboard: "board".into(),
                slot: "centre".into(),
                team: None,
                scope: "open".into(),
            },
            temp.path().join("own").to_string_lossy().into(),
            temp.path().join("own/board.html").to_string_lossy().into(),
            temp.path().join("own").to_string_lossy().into(),
            FramePolicy {
                own_folder_only: true,
                ..Default::default()
            },
            None,
        );
        let root = URL_SAFE_NO_PAD.encode(temp.path().to_string_lossy().as_bytes());
        let app_origins =
            crate::http::app_origins::AppOrigins::from_dev_url(Some("http://localhost:5328"));
        assert_eq!(
            serve_baseline_content(
                &root,
                "own/board.html",
                ViewerQuery {
                    theme: None,
                    v: None,
                    tree: Some(oid.to_string())
                },
                &frame,
                &app_origins,
            )
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
        let record_baseline = serve_baseline_content(
            &root,
            "own/board.html",
            ViewerQuery {
                theme: None,
                v: None,
                tree: Some(oid.to_string()),
            },
            &frame,
            &app_origins,
        )
        .await
        .unwrap();
        assert!(record_baseline
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .is_some_and(|csp| csp.to_str().unwrap().contains("sandbox allow-scripts")));
        assert_eq!(
            record_baseline.headers()[header::CONTENT_SECURITY_POLICY],
            format!("sandbox allow-scripts; {}", app_origins.frame_ancestors())
        );
        std::fs::remove_file(temp.path().join("own/board.html")).unwrap();
        assert_eq!(
            serve_baseline_content(
                &root,
                "own/board.html",
                ViewerQuery {
                    theme: None,
                    v: None,
                    tree: Some(oid.to_string())
                },
                &frame,
                &app_origins,
            )
            .await
            .unwrap()
            .status(),
            StatusCode::OK
        );
        for message_safe in [false, true] {
            if message_safe {
                frame.policy.own_folder_only = false;
                frame.policy.team_safe = true;
                frame.message = Some(FrameMessage {
                    org: "acme".into(),
                    team: "own".into(),
                    message_id: 7,
                });
            }
            for foreign in ["foreign/board.html", "private/board.html"] {
                let query = || ViewerQuery {
                    theme: None,
                    v: None,
                    tree: Some(oid.to_string()),
                };
                assert_eq!(
                    serve_baseline_content(&root, foreign, query(), &frame, &app_origins)
                        .await
                        .unwrap_err(),
                    StatusCode::FORBIDDEN
                );
            }
        }
    }

    // Runs only when the browser suite explicitly asks for its isolated server.
    #[test]
    fn child_message_shim_talks_to_the_host_without_rewriting_authored_bytes() {
        let authored = b"<button>parent</button><script>window.parent.authored()</script>";
        let html = String::from_utf8(live_doc_body_for_host(
            String::new(),
            authored,
            MESSAGE_SIZE_JS,
            true,
        ))
        .unwrap();
        assert!(html.contains("window.top.postMessage"));
        assert!(html.contains("e.source!==window.top"));
        assert!(html.ends_with(std::str::from_utf8(authored).unwrap()));
    }

    #[cfg(not(feature = "e2e-webdriver"))]
    #[test]
    fn release_shim_has_no_probe() {
        let html = String::from_utf8(live_doc_body_for_host(
            String::new(),
            b"<p>x</p>",
            "",
            false,
        ))
        .unwrap();
        assert!(!html.contains("codesense:probe"));
    }

    #[cfg(feature = "e2e-webdriver")]
    #[test]
    fn e2e_shim_answers_probe_from_the_parent_only() {
        let html = String::from_utf8(live_doc_body_for_host(
            String::new(),
            b"<p>x</p>",
            "",
            false,
        ))
        .unwrap();
        assert!(html.contains("codesense:probe-result"));
        assert!(html.contains("if(e.source!==window.parent)return;var d=e.data;if(!d||d.type!=="));
        let child = String::from_utf8(live_doc_body_for_host(String::new(), b"<p>x</p>", "", true))
            .unwrap();
        assert!(child.contains("if(e.source!==window.top)return;var d=e.data;if(!d||d.type!=="));
    }

    #[tokio::test]
    async fn hostile_fixture_server() {
        let Ok(info_file) = std::env::var("CODESENSE_FRAME_FIXTURE") else {
            return;
        };
        use crate::dashboard_stream::frame::{FrameGrant, FramePolicy, FrameStore};
        use crate::http::frame_boundary::{with_boundary, FrameBoundary};
        let (v4, v6) = crate::http::bind_api_listener(0, None).await.unwrap();
        let port = v4.local_addr().unwrap().port();
        let frames = std::sync::Arc::new(FrameStore::default());
        let context = std::path::Path::new(&info_file)
            .parent()
            .unwrap()
            .join("context");
        let mut records = Vec::new();
        for name in [
            "team",
            "private",
            "open",
            "sw-blocked",
            "net-blocked",
            "baseline",
        ] {
            let folder = context.join(name);
            std::fs::create_dir_all(&folder).unwrap();
            let file = folder.join(format!("{name}.html"));
            std::fs::write(
                &file,
                format!("<body data-private='private-bytes'>{name}</body>"),
            )
            .unwrap();
            std::fs::write(folder.join("data.txt"), "private-bytes").unwrap();
            std::fs::write(folder.join("data.json"), r#"{"value":1}"#).unwrap();
            std::fs::write(
                folder.join("silence.wav"),
                include_bytes!("../../../../tests/frame-security/fixtures/silence.wav"),
            )
            .unwrap();
            std::fs::write(
                folder.join("black.mp4"),
                include_bytes!("../../../../tests/frame-security/fixtures/black.mp4"),
            )
            .unwrap();
            std::fs::write(
                folder.join("captions.vtt"),
                "WEBVTT\n\n00:00.000 --> 00:00.500\nfixture\n",
            )
            .unwrap();
            records.push(
                frames.mint(
                    port,
                    FrameGrant {
                        frame_id: String::new(),
                        principal: name.into(),
                        dashboard: file.to_string_lossy().into_owned(),
                        slot: if name == "baseline" {
                            "baseline"
                        } else {
                            "centre"
                        }
                        .into(),
                        team: None,
                        scope: "open".into(),
                    },
                    context.to_string_lossy().into_owned(),
                    file.to_string_lossy().into_owned(),
                    folder.to_string_lossy().into_owned(),
                    FramePolicy {
                        own_folder_only: name == "team" || name == "baseline",
                        block_service_workers: name == "sw-blocked",
                        no_outside_network: name == "net-blocked",
                        allowed_hosts: std::env::var("CODESENSE_FIXTURE_ALLOWED_ORIGIN")
                            .ok()
                            .into_iter()
                            .collect(),
                        ..Default::default()
                    },
                    None,
                ),
            );
        }
        let inline_folder = context.join("inline");
        std::fs::create_dir_all(&inline_folder).unwrap();
        let parent_path = inline_folder.join("pane.html");
        let parent = frames.mint(
            port,
            FrameGrant {
                frame_id: String::new(),
                principal: "human".into(),
                dashboard: "inbox".into(),
                slot: "right-pane".into(),
                team: None,
                scope: "open".into(),
            },
            context.to_string_lossy().into_owned(),
            parent_path.to_string_lossy().into_owned(),
            inline_folder.to_string_lossy().into_owned(),
            FramePolicy::default(),
            None,
        );
        let child_path = inline_folder.join("message.html");
        let child = frames.mint(
            port,
            FrameGrant {
                frame_id: String::new(),
                principal: "Mia".into(),
                dashboard: "inbox-message://acme/mail/7".into(),
                slot: "message".into(),
                team: Some(crate::dashboard_stream::frame::FrameTeam {
                    org: "acme".into(),
                    team: "mail".into(),
                }),
                scope: "open".into(),
            },
            context.to_string_lossy().into_owned(),
            child_path.to_string_lossy().into_owned(),
            inline_folder.to_string_lossy().into_owned(),
            FramePolicy::default(),
            Some(crate::dashboard_stream::frame::FrameMessage {
                org: "acme".into(),
                team: "mail".into(),
                message_id: 7,
            }),
        );
        let child = frames
            .link_child(&child.frame_id, &parent.frame_id)
            .unwrap();
        let root = URL_SAFE_NO_PAD.encode(context.to_string_lossy().as_bytes());
        std::fs::write(&parent_path, format!("<body style='color:rgb(0, 0, 255)'><p id='pane-marker'>Pane</p><iframe id='inline' sandbox='allow-scripts allow-same-origin' src='{}/viewer/{root}/inline/message.html'></iframe></body>", child.origin)).unwrap();
        std::fs::write(&child_path, "<style>body{color:rgb(255,0,0)}</style><button id='reply' onclick=\"codesense.act('replyAsHuman',{messageId:7,text:'Reply'}).then(r=>document.body.dataset.result=r.identity)\">Reply</button>").unwrap();
        records.extend([parent, child]);
        std::fs::write(
            context.join("private/outside.js"),
            "window.loopbackLeak='private-bytes'",
        )
        .unwrap();
        std::fs::write(
            context.join("baseline/baseline.html"),
            "<body>baseline<script src='./own.js'></script></body>",
        )
        .unwrap();
        std::fs::write(
            context.join("baseline/own.js"),
            "parent.postMessage({type:'baseline-rendered',ready:true},'*')",
        )
        .unwrap();
        // Committed baseline HTML attempts to relay a private committed blob.
        std::fs::write(
            context.join("team/baseline.html"),
            r#"<p>baseline</p><script>
          fetch('../private/data.txt').then(r=>r.text()).catch(()=> 'denied').then(bytes=>
            parent.postMessage({type:'baseline-read',bytes:bytes},'*'));
        </script>"#,
        )
        .unwrap();
        {
            let repo = git2::Repository::init(&context).unwrap();
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            let oid = index.write_tree().unwrap();
            let tree = repo.find_tree(oid).unwrap();
            let sig = git2::Signature::now("fixture", "fixture@example.com").unwrap();
            let commit = repo
                .commit(Some("HEAD"), &sig, &sig, "baseline fixture", &tree, &[])
                .unwrap();
            repo.reference("refs/remotes/origin/main", commit, true, "fixture baseline")
                .unwrap();
        }
        async fn document(
            axum::Extension(frame): axum::Extension<FrameRecord>,
            Path((root, path)): Path<(String, String)>,
            headers: HeaderMap,
        ) -> Result<Response, StatusCode> {
            let file = resolve_viewer_path(&root, &path)?;
            serve_frame_file(
                &file,
                &frame,
                &headers,
                None,
                |_| String::new(),
                |real| {
                    let root = std::path::Path::new(&frame.content_root)
                        .canonicalize()
                        .map_err(|_| StatusCode::FORBIDDEN)?;
                    if real.starts_with(root) {
                        Ok(())
                    } else {
                        Err(StatusCode::FORBIDDEN)
                    }
                },
            )
            .await
        }

        async fn baseline(
            State(state): State<FrameBoundary>,
            Path((root, path)): Path<(String, String)>,
            Query(query): Query<ViewerQuery>,
            headers: HeaderMap,
        ) -> Result<Response, StatusCode> {
            // Use the same production baseline byte handler, with only the
            // isolated test context standing in for the app's context allowlist.
            let normalized = resolve_viewer_path(&root, &path)?;
            let repo = PathBuf::from(decode_viewer_root(&root)?);
            if !normalized.starts_with(&repo) || !repo.join(".git").is_dir() {
                return Err(StatusCode::FORBIDDEN);
            }
            let frame = headers
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .and_then(|host| state.frames.by_host(host, state.port))
                .ok_or(StatusCode::FORBIDDEN)?;
            serve_baseline_content(&root, &path, query, &frame, &state.app_origins).await
        }

        async fn read(
            frame: Option<axum::Extension<FrameRecord>>,
            State(state): State<FrameBoundary>,
            headers: HeaderMap,
            axum::extract::RawQuery(raw): axum::extract::RawQuery,
        ) -> Result<axum::Json<serde_json::Value>, StatusCode> {
            let Some(axum::Extension(frame)) = frame else {
                let shared = crate::http::ProxyCapabilityState("process-secret".into());
                if !shared.matches(
                    headers
                        .get("x-codesense-proxy-token")
                        .and_then(|h| h.to_str().ok())
                        .unwrap_or(""),
                ) {
                    return Err(StatusCode::UNAUTHORIZED);
                }
                return Ok(axum::Json(serde_json::json!({"identity":"cli"})));
            };
            let grant =
                super::super::read_gate::frame_read_grant(&state.frames, state.port, &headers)
                    .ok_or(StatusCode::UNAUTHORIZED)??;
            Ok(axum::Json(serde_json::json!({
                "identity":frame.grant.principal,"frameId":frame.frame_id,
                "contentRoot":super::super::read_gate::select_requested_root(Some(&grant),
                    super::super::read_gate::parse_raw_query(raw.as_deref()).content_root.as_deref())
            })))
        }
        async fn ancestors(mut response: Response) -> Response {
            let policies = response.headers().get_all(header::CONTENT_SECURITY_POLICY).iter()
                .filter_map(|v| v.to_str().ok())
                .map(|csp| csp.replace("frame-ancestors tauri://localhost http://tauri.localhost https://tauri.localhost", "frame-ancestors http://127.0.0.1:*"))
                .collect::<Vec<_>>();
            response
                .headers_mut()
                .remove(header::CONTENT_SECURITY_POLICY);
            for policy in policies {
                response.headers_mut().append(
                    header::CONTENT_SECURITY_POLICY,
                    HeaderValue::from_str(&policy).unwrap(),
                );
            }
            response
        }
        let boundary = FrameBoundary {
            frames,
            port,
            app_origins: crate::http::app_origins::AppOrigins::default(),
        };
        let app = with_boundary(
            Router::new()
                .merge(super::super::dashboard_kit::router())
                .route("/viewer/{root}/{*path}", get(document))
                .route("/viewer-baseline/{root}/{*path}", get(baseline))
                .route("/api/v1/sprints", get(read))
                .route("/api/v1/taxonomy", get(read))
                .route("/network-worker.js", get(|| async { (
                    [(header::CONTENT_TYPE, "application/javascript")],
                    "self.addEventListener('install',()=>self.skipWaiting());self.addEventListener('message',e=>{fetch(e.data).then(r=>r.text()).catch(()=> 'blocked').then(t=>e.ports[0].postMessage(t))})"
                ) }))
                .route("/health", get(|| async { "ok" }))
                .route("/api/v1/terminal/status", axum::routing::post(|| async { "ack" }))
                .route(
                    "/worker.js",
                    get(|| async {
                        (
                            [(header::CONTENT_TYPE, "application/javascript")],
                            "self.addEventListener('install',()=>self.skipWaiting())",
                        )
                    }),
                )
                .with_state(boundary.clone()),
            boundary,
        )
        .layer(middleware::map_response(ancestors));
        std::fs::write(
            info_file,
            serde_json::to_string(&serde_json::json!({"port":port,"records":records})).unwrap(),
        )
        .unwrap();
        let ipv6_app = app.clone();
        tokio::spawn(async move {
            axum::serve(v6, ipv6_app).await.unwrap();
        });
        axum::serve(v4, app).await.unwrap();
    }
}

async fn serve_file_request<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    root_b64url: &str,
    rel_path: &str,
    headers: &HeaderMap,
    theme: Option<&str>,
) -> Result<Response, StatusCode> {
    let frame =
        crate::http::frame_boundary::request_frame(app, &headers).ok_or(StatusCode::FORBIDDEN)?;
    if frame.policy.own_folder_only && rel_path.split('/').any(|p| p == "..") {
        return Err(StatusCode::FORBIDDEN);
    }
    let normalized = resolve_viewer_path(&root_b64url, &rel_path)?;

    // Security: validate path is within allowed context directories.
    validate_frame_path(app, &frame, &normalized)?;

    // 404 if file doesn't exist or is a directory.
    if !normalized.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }

    serve_frame_file(
        &normalized,
        &frame,
        &headers,
        theme,
        |html| frame_first_paint(app, &frame, html),
        |real| validate_frame_path(app, &frame, real),
    )
    .await
}

async fn serve_pane_request<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    content_b64url: &str,
    panes_b64url: &str,
    rel_path: &str,
    headers: &HeaderMap,
    theme: Option<&str>,
) -> Result<Response, StatusCode> {
    let frame =
        crate::http::frame_boundary::request_frame(app, &headers).ok_or(StatusCode::FORBIDDEN)?;
    if frame.policy.own_folder_only && rel_path.split('/').any(|p| p == "..") {
        return Err(StatusCode::FORBIDDEN);
    }
    let file = resolve_pane_path(&panes_b64url, &rel_path)?;
    validate_frame_path(app, &frame, &file)?;
    if !file.is_file() {
        return Err(StatusCode::NOT_FOUND);
    }
    // A team's panes/ arrives by git, so a link in it is a teammate's: it may
    // point within panes/ or at other context files, never at the rest of the
    // disk (a key, a dotfile) for a pane to read and send on.
    let real = file.canonicalize().map_err(|_| StatusCode::NOT_FOUND)?;
    if real != file
        && !link_target_allowed_for(
            &file,
            &real,
            app,
            &crate::context_repo::caller_scope::for_page(
                app,
                Some(&frame.document_path),
                Some(&frame.frame_id),
            )
            .map_err(|_| StatusCode::NOT_FOUND)?,
        )
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let content_root = decode_viewer_root(&content_b64url)?;
    // The read capability is bound to this root, so it must be a context dir
    // too, never an arbitrary directory a doc names.
    validate_frame_path(app, &frame, std::path::Path::new(&content_root))?;
    serve_frame_file(
        &file,
        &frame,
        &headers,
        theme,
        |html| frame_first_paint(app, &frame, html),
        |real| validate_frame_path(app, &frame, real),
    )
    .await
}

async fn serve_baseline_file_request<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    root_b64url: &str,
    rel_path: &str,
    query: ViewerQuery,
    headers: &HeaderMap,
    app_origins: &crate::http::app_origins::AppOrigins,
) -> Result<Response, StatusCode> {
    let frame =
        crate::http::frame_boundary::request_frame(app, &headers).ok_or(StatusCode::FORBIDDEN)?;
    // Defense in depth: validate the resolved working-tree path is within an
    // allowed context dir (same guard as serve_file), even though the bytes
    // come from git rather than disk.
    let normalized = resolve_viewer_path(&root_b64url, &rel_path)?;
    validate_frame_path(app, &frame, &normalized)?;

    serve_baseline_content(&root_b64url, &rel_path, query, &frame, app_origins).await
}

fn validate_frame_path<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    frame: &FrameRecord,
    path: &std::path::Path,
) -> Result<(), StatusCode> {
    let scope = crate::context_repo::caller_scope::for_page(
        app,
        Some(&frame.document_path),
        Some(&frame.frame_id),
    )
    .map_err(|_| StatusCode::NOT_FOUND)?;
    scope.require(path).map_err(|_| StatusCode::NOT_FOUND)?;
    crate::commands::files::validate_context_path_for(path, app, &scope)
        .map_err(|_| StatusCode::FORBIDDEN)
}

#[cfg(test)]
mod caller_scope_tests {
    use super::*;
    use crate::context_repo::caller_scope::tests::AppFixture;
    use tower::ServiceExt;
    #[test]
    fn authenticated_byte_routes_keep_the_frame_team_when_active_team_changes() {
        let f = AppFixture::new();
        f.app.manage(crate::http::HttpApiPort(9321));
        let shared = f
            .fixture
            .write("parent", "panes/shared.json", "shared asset bytes");
        f.fixture
            .write("parent", "panes/secret.json", "hidden asset sentinel");
        f.fixture.write(
            "parent",
            "config/sharing.yml",
            "share:\n  - panes/parent.html\n  - panes/shared.json\n",
        );
        let child = f.frame(&f.fixture.path("parent", "panes/parent.html"), "workspace");
        let frames = f
            .app
            .state::<std::sync::Arc<crate::dashboard_stream::frame::FrameStore>>()
            .inner()
            .clone();
        let mut grant = child.grant.clone();
        grant.team.as_mut().unwrap().team = "parent".into();
        let parent = frames.mint(
            9321,
            grant,
            f.fixture.path("parent", "content").display().to_string(),
            child.document_path.clone(),
            child.asset_folder.clone(),
            child.policy.clone(),
            None,
        );
        let repo = git2::Repository::init(&f.fixture.root).unwrap();
        let mut index = repo.index().unwrap();
        for path in [
            "parent/panes/parent.html",
            "parent/panes/shared.json",
            "parent/panes/secret.json",
        ] {
            index.add_path(std::path::Path::new(path)).unwrap();
        }
        let oid = index.write_tree().unwrap();
        let tree = repo.find_tree(oid).unwrap();
        let sig = git2::Signature::now("fixture", "fixture@example.com").unwrap();
        let commit = repo
            .commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
            .unwrap();
        repo.reference("refs/remotes/origin/main", commit, true, "fixture")
            .unwrap();
        type App = tauri::AppHandle<tauri::test::MockRuntime>;
        async fn file(
            State(app): State<App>,
            Path((root, path)): Path<(String, String)>,
            headers: HeaderMap,
        ) -> Result<Response, StatusCode> {
            serve_file_request(&app, &root, &path, &headers, None).await
        }
        async fn pane(
            State(app): State<App>,
            Path((content, root, path)): Path<(String, String, String)>,
            headers: HeaderMap,
        ) -> Result<Response, StatusCode> {
            serve_pane_request(&app, &content, &root, &path, &headers, None).await
        }
        async fn baseline(
            State(app): State<App>,
            Path((root, path)): Path<(String, String)>,
            Query(query): Query<ViewerQuery>,
            headers: HeaderMap,
        ) -> Result<Response, StatusCode> {
            serve_baseline_file_request(
                &app,
                &root,
                &path,
                query,
                &headers,
                &crate::http::app_origins::AppOrigins::default(),
            )
            .await
        }
        let boundary = crate::http::frame_boundary::FrameBoundary {
            frames,
            port: 9321,
            app_origins: crate::http::app_origins::AppOrigins::default(),
        };
        let router = crate::http::frame_boundary::with_boundary(
            Router::new()
                .route("/viewer/{root}/{*path}", get(file))
                .route("/pane/{content}/{root}/{*path}", get(pane))
                .route("/viewer-baseline/{root}/{*path}", get(baseline))
                .with_state(f.app.handle().clone()),
            boundary,
        );
        tauri::async_runtime::block_on(async {
            for (frame, active, own) in [(&child, "parent", false), (&parent, "child", true)] {
                f.app.state::<crate::mcp::state::ActiveTeamState>().update(
                    crate::mcp::state::ActiveTeam {
                        org_slug: "acme/ctx".into(),
                        team_name: active.into(),
                    },
                );
                let encode =
                    |p: &std::path::Path| URL_SAFE_NO_PAD.encode(p.to_string_lossy().as_bytes());
                let prefixes = [
                    format!("/viewer/{}", encode(shared.parent().unwrap())),
                    format!(
                        "/pane/{}/{}",
                        encode(std::path::Path::new(&frame.content_root)),
                        encode(shared.parent().unwrap())
                    ),
                    format!("/viewer-baseline/{}/parent/panes", encode(&f.fixture.root)),
                ];
                for prefix in prefixes {
                    let read = |name: &str| {
                        router.clone().oneshot(
                            axum::http::Request::builder()
                                .uri(format!("{prefix}/{name}"))
                                .header(header::HOST, frame.origin.trim_start_matches("http://"))
                                .header(header::ORIGIN, &frame.origin)
                                .header("x-codesense-proxy-token", &frame.token)
                                .body(Body::empty())
                                .unwrap(),
                        )
                    };
                    let denied = read("secret.json").await.unwrap();
                    if own {
                        assert_eq!(denied.status(), StatusCode::OK);
                        assert_eq!(
                            axum::body::to_bytes(denied.into_body(), 1024)
                                .await
                                .unwrap(),
                            "hidden asset sentinel"
                        );
                    } else {
                        let missing = read("missing.json").await.unwrap();
                        assert_eq!(denied.status(), missing.status());
                        assert_eq!(denied.status(), StatusCode::NOT_FOUND);
                        assert_eq!(
                            axum::body::to_bytes(denied.into_body(), 1024)
                                .await
                                .unwrap(),
                            axum::body::to_bytes(missing.into_body(), 1024)
                                .await
                                .unwrap()
                        );
                    }
                    let positive = read("shared.json").await.unwrap();
                    assert_eq!(positive.status(), StatusCode::OK);
                    assert_eq!(
                        axum::body::to_bytes(positive.into_body(), 1024)
                            .await
                            .unwrap(),
                        "shared asset bytes"
                    );
                }
            }
        });
    }
}
