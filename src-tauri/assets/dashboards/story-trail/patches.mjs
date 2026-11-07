// SPDX-License-Identifier: MIT
// kit.keyed owns reconciliation. This layer only postpones protected structure.
export function createPatches({root,kit,doc=root.ownerDocument}) {
  const records=new WeakMap(),pending=new Map(),texts=new Map(),refreshes=new Map();
  let gesturing=false,stopped=false,releaseTimer=null;
  const key=(opts,item)=>String(opts.key(item));
  const version=(opts,item)=>String(opts.version?.(item)??'row-v1');
  function selected(node){const selection=doc.getSelection();if(!selection?.rangeCount||selection.isCollapsed||!node.isConnected)return false;return selection.getRangeAt(0).intersectsNode(node);}
  function associated(node){const id=node?.closest?.('[data-row-id]')?.dataset.rowId;return id?root.querySelector('[data-row-focus="'+CSS.escape(id)+'"]'):null;}
  function touching(node){const active=doc.hasFocus()?doc.activeElement:null;return !!(active&&(node.contains(active)||node.contains(associated(active)))||selected(node));}
  function text(node,value){value=String(value??'');if(node.textContent===value){texts.delete(node);return;}if(selected(node)){texts.set(node,value);return;}if(node.childNodes.length===1&&node.firstChild.nodeType===3)node.firstChild.data=value;else node.textContent=value;texts.delete(node);}
  function keyed(container,items,opts,passive=false){
    const previous=records.get(container),incoming=new Map(items.map(item=>[key(opts,item),item]));
    const order=items.map(item=>key(opts,item));
    const structural=previous&&(order.join('\0')!==previous.order.join('\0')||items.some(item=>previous.versions.get(key(opts,item))!==version(opts,item)));
    // A genuine deletion of the protected row is handled by the focus guardian.
    const protectedRow=[...container.children].some(node=>incoming.has(node.dataset.key)&&touching(node));
    let effective=items,versions=new Map(items.map(item=>[key(opts,item),version(opts,item)]));
    if(passive&&structural&&protectedRow){
      pending.set(container,{items,opts});effective=previous.items.map(item=>incoming.get(key(opts,item))??item);versions=previous.versions;
    }else pending.delete(container);
    kit.keyed(container,effective,{key:opts.key,version:item=>versions.get(key(opts,item))??version(opts,item),render:item=>{const node=opts.render(item);opts.update?.(node,item);return node;},update:opts.update});
    records.set(container,{items:effective,order:effective.map(item=>key(opts,item)),versions});
  }
  // Gestures protect the whole page: even facts in another row can move a target.
  // Refresh callbacks read current controller state when released, after the click.
  function defer(owner,refresh){if(!gesturing)return false;refreshes.set(owner,refresh);return true;}
  function flush(){if(stopped||gesturing)return;const work=[...refreshes.values()];refreshes.clear();for(const refresh of work)refresh();for(const [node,value]of texts){if(!node.isConnected)texts.delete(node);else if(!selected(node))text(node,value);}for(const [container,work]of [...pending]){if(container.isConnected)keyed(container,work.items,work.opts,true);else pending.delete(container);}}
  const down=event=>{
    if(event.button!==0||event.isPrimary===false)return;
    if(releaseTimer!=null){clearTimeout(releaseTimer);releaseTimer=null;}
    gesturing=true;
  };
  // The click handler consumes the held target before queued reads can patch it.
  const end=()=>{if(releaseTimer!=null){clearTimeout(releaseTimer);releaseTimer=null;}gesturing=false;queueMicrotask(flush);};
  const up=()=>{if(gesturing&&releaseTimer==null)releaseTimer=setTimeout(end,0);};
  const exit=event=>{if(gesturing&&event.buttons&&!event.relatedTarget)end();};
  const leave=()=>queueMicrotask(flush);
  doc.addEventListener('pointerdown',down,true);doc.addEventListener('pointerup',up,true);doc.addEventListener('pointerleave',exit,true);doc.addEventListener('mouseleave',exit,true);doc.addEventListener('click',end);doc.addEventListener('auxclick',end,true);doc.addEventListener('dragend',end,true);doc.addEventListener('pointercancel',end,true);doc.addEventListener('focusout',leave,true);doc.addEventListener('selectionchange',leave);doc.defaultView.addEventListener('blur',end);
  function stop(){stopped=true;end();pending.clear();texts.clear();refreshes.clear();doc.removeEventListener('pointerdown',down,true);doc.removeEventListener('pointerup',up,true);doc.removeEventListener('pointerleave',exit,true);doc.removeEventListener('mouseleave',exit,true);doc.removeEventListener('click',end);doc.removeEventListener('auxclick',end,true);doc.removeEventListener('dragend',end,true);doc.removeEventListener('pointercancel',end,true);doc.removeEventListener('focusout',leave,true);doc.removeEventListener('selectionchange',leave);doc.defaultView.removeEventListener('blur',end);}
  return {keyed,text,defer,stop,flush};
}
