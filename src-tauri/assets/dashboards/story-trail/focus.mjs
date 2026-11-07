// SPDX-License-Identifier: MIT
// Capture meaning before replacing DOM; every restoration uses the same fallback chain.
export function createFocusIntents({root,doc=root.ownerDocument,context=()=>({})}) {
  let movement=0,restoring=false;
  const attr=(name,value)=>`[${name}="${CSS.escape(String(value))}"]`;
  const available=el=>el&&!el.disabled&&!el.closest('[hidden], [inert]');
  const controls=row=>[...row.querySelectorAll('button,a[href],input,select,textarea,summary,[tabindex]')].filter(el=>available(el)&&el.tabIndex>=0);
  const find=(scope,selector)=>selector?(scope.matches?.(selector)?scope:scope.querySelector(selector)):null;
  function point(node,offset){
    const parent=node.nodeType===1?node:node.parentElement;
    const scope=parent?.closest('[data-row-focus],[id]')??root;
    const key=scope.id?`#${CSS.escape(scope.id)}`:scope.dataset?.rowFocus?attr('data-row-focus',scope.dataset.rowFocus):null;
    const path=[];for(let at=node;at!==scope;at=at.parentNode){if(!at.parentNode)return null;path.unshift([...at.parentNode.childNodes].indexOf(at));}
    return {key,path,offset};
  }
  function textSelection(){
    const selected=doc.getSelection();if(!selected||selected.isCollapsed||!root.contains(selected.anchorNode)||!root.contains(selected.focusNode))return null;
    const range=selected.getRangeAt(0);
    return {start:point(range.startContainer,range.startOffset),end:point(range.endContainer,range.endOffset),text:selected.toString(),forward:selected.anchorNode===range.startContainer&&selected.anchorOffset===range.startOffset};
  }
  function restoreSelection(saved){
    if(!saved?.start||!saved.end)return;
    const resolve=point=>{let node=point.key?root.querySelector(point.key):root;if(point.key&&root.matches?.(point.key))node=root;for(const i of point.path)node=node?.childNodes[i];return node&&point.offset<=(node.nodeType===3?node.length:node.childNodes.length)?node:null;};
    const start=resolve(saved.start),end=resolve(saved.end);if(!start||!end)return;
    const range=doc.createRange();range.setStart(start,saved.start.offset);range.setEnd(end,saved.end.offset);if(range.toString()!==saved.text)return;
    const selected=doc.getSelection();if(saved.forward)selected.setBaseAndExtent(start,saved.start.offset,end,saved.end.offset);else selected.setBaseAndExtent(end,saved.end.offset,start,saved.start.offset);
  }
  function capture(element=doc.activeElement){
    const selection=textSelection();
    if(!element||element===doc.body||!root.contains(element))return {focused:false,selection};
    const extra=context(element),row=element.closest('[data-row-focus]'),task=element.closest('[data-task]');
    const rowId=extra.rowId??row?.dataset.rowFocus??(task?`task:${task.dataset.task}`:null);
    const sectionId=extra.sectionId??element.closest('section')?.querySelector('h2[id]')?.id??(rowId?.startsWith('task:')?'tasks-title':rowId?.startsWith('session:')?'sessions-title':null);
    const key=['data-focus-key','data-panel-key','data-sort','data-open','data-retry','data-flat','data-group','data-task','data-cancel-rate','data-close','data-conv-action','data-jump','data-lines-path','name'].find(name=>element.hasAttribute(name));
    let controlId=key?attr(key,element.getAttribute(key)):element.id?`#${CSS.escape(element.id)}`:element.matches('button[type=submit]')?'#rate-form [type=submit]':null;
    if(element.tagName==='SUMMARY'&&element.closest('[data-tool-key]'))controlId=`${attr('data-tool-key',element.closest('[data-tool-key]').dataset.toolKey)} summary`;
    return {movement,selection,rowId,controlId,sectionId,position:row?Math.max(0,controls(row).indexOf(element)):0,element};
  }
  // Resolve before replacing DOM: a completion cannot override newer user intent.
  function completion(intent,{since,reveal=false}={}){
    const active=doc.activeElement;
    if(!doc.hasFocus())return {intent:{...capture(),focused:false},reveal:false};
    const allowed=since&&since.movement===movement&&(active===since.element||active===doc.body);
    return {intent:allowed?intent:capture(),reveal:!!allowed&&reveal};
  }
  function restore(intent,{reveal=false,revealEl=null}={}){
    if(!intent)return;
    restoreSelection(intent.selection);if(intent.focused===false)return;
    const rowNode=intent.rowId?root.querySelector(attr('data-row-focus',intent.rowId)):null;
    const row=available(rowNode)?rowNode:null;
    let target=available(intent.element)&&root.contains(intent.element)?intent.element:null;
    target??=find(row??root,intent.controlId);
    // Form and panel controls can live outside their originating row.
    if(!available(target))target=null;
    if(!target&&intent.controlId&&!intent.controlId.startsWith('[data-rate='))target=find(root,intent.controlId);
    if(!available(target))target=null;
    let fallback=false;
    if(!target&&row){fallback=true;const candidates=controls(row);target=candidates[Math.min(intent.position??0,candidates.length-1)]??row;}
    const sectionId=intent.sectionId??(intent.rowId?.startsWith('task:')?'tasks-title':intent.rowId?.startsWith('session:')?'sessions-title':null);
    if(!target)fallback=true;
    target??=(sectionId?doc.getElementById(sectionId):null)??root.querySelector('h1,h2');
    if(target){if(target.tabIndex<0)target.setAttribute('tabindex','-1');restoring=true;try{target.focus({preventScroll:true});}finally{restoring=false;}if(reveal||fallback){const anchor=(typeof revealEl==='string'?target.querySelector(revealEl):revealEl)??target.querySelector(':scope > [data-row-label]')??target;anchor.scrollIntoView?.({block:'nearest'});}}
  }
  // Surviving controls keep their native focus; only explicit moves or loss restore.
  function reconcile(intent,options={}){
    if(intent?.focused===false)return;
    if(options.reveal||intent&&(doc.activeElement===doc.body||!root.contains(intent.element)||!available(intent.element)))restore(intent,options);
  }
  function captureReading(scope){
    return [...scope.querySelectorAll('[data-scroll-key]')].map(node=>({key:node.dataset.scrollKey,x:node.scrollLeft,y:node.scrollTop}));
  }
  function restoreReading(scope,{target=null,saved=[]}={}){
    // Page targets reset outer scrollers; nested tool reading stays keyed to its page.
    for(const node of scope.querySelectorAll('[data-scroll-key]')){
      const position=target&&!node.closest('[data-tool-key]')?null:saved.find(item=>item.key===node.dataset.scrollKey);
      node.scrollLeft=position?.x??0;node.scrollTop=position?.y??0;
    }
    if(target&&!target.hasAttribute('data-scroll-key'))target.scrollIntoView?.({block:'center'});
  }
  const view=doc.defaultView;
  let lastIntent=null,guardianFrame=null;
  const remember=event=>{const intent=capture(event.target);if(intent&&intent.focused!==false)lastIntent=intent;};
  const guard=event=>{
    if(event?.target)remember(event);
    // Native date editors can omit focusin; focusout still names the destination.
    if(event?.relatedTarget){if(root.contains(event.relatedTarget))remember({target:event.relatedTarget});return;}
    if(guardianFrame!=null)return;
    guardianFrame=view.requestAnimationFrame(()=>{
      guardianFrame=null;
      // A deliberate Tab/Escape into the host must remain outside this document.
      if(lastIntent&&doc.hasFocus()&&(doc.activeElement===doc.body||!root.contains(doc.activeElement))){if(root.contains(lastIntent.element)&&available(lastIntent.element)){lastIntent=null;return;}restore({...lastIntent,selection:null});}
    });
  };
  const entered=event=>{if(!restoring)movement++;remember(event);};
  const pointed=event=>{if(!doc.activeElement?.contains(event.target))movement++;};
  root.addEventListener('pointerdown',pointed,true);root.addEventListener('focusin',entered,true);root.addEventListener('focusout',guard,true);
  // WebKit does not emit focusout when the focused node is removed.
  const observer=new view.MutationObserver(()=>{if(lastIntent&&(!root.contains(lastIntent.element)||!available(lastIntent.element))&&doc.hasFocus()&&(doc.activeElement===doc.body||!root.contains(doc.activeElement)))guard();});
  observer.observe(root,{childList:true,subtree:true});
  function stop(){observer.disconnect();root.removeEventListener('pointerdown',pointed,true);root.removeEventListener('focusin',entered,true);root.removeEventListener('focusout',guard,true);if(guardianFrame!=null)view.cancelAnimationFrame(guardianFrame);lastIntent=null;}
  return {capture,completion,restore,reconcile,captureReading,restoreReading,stop};
}
