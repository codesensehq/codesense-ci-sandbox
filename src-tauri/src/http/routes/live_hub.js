// SPDX-License-Identifier: MIT
(function () {
  var c = window.codesense;
  if (!c) return;
  c.files = {};
  ['list','read','move','delete','create'].forEach(function(operation){c.files[operation]=function(args){return c.invoke('files.'+operation,args||{});};});
  c.records = {};
  ['collections','list','read','search','history','readAt','create','write','setFields','delete','restore'].forEach(function(operation){c.records[operation]=function(args){return c.invoke('records.'+operation,args||{});};});
  var topics = new Map(), visible = false, visibilityKnown = false, badgeLive = false;
  var invoke = c.invoke.bind(c);
  var parentOrigin;
  // Architecture: the host-resolved declaration is the single source for hidden badge eligibility.
  function liveBadge() { return badgeLive; }
  function active() { return visible || liveBadge(); }
  function whenVisible(run) {
    if (visibilityKnown && active()) return run();
    return new Promise(function (resolve,reject) {
      var off = c.onVisibility(function (shown) {
        if (shown || liveBadge()) queueMicrotask(function () { off(); run().then(resolve,reject); });
      });
    });
  }
  c.invoke = function (verb, args) {
    if (verb === 'subscribe') return invoke(verb,args);
    // Hiding and retirement still need to persist the final edit and its draft mirror.
    if (verb === 'records.write' || verb === 'setPrefs') return invoke(verb,args);
    return whenVisible(function () { return invoke(verb,args); });
  };
  ['sprints', 'taxonomy'].forEach(function (name) {
    var read = c[name];
    c[name] = function () { return whenVisible(function () { return read.call(c); }); };
  });
  function rowId(row) { return row.id || row.handle; }
  function orderedRows(topic, rows) { return rows.sort(function(a,b){if(topic==='files')return Number(b.kind==='dir')-Number(a.kind==='dir')||String(a.name).localeCompare(String(b.name),undefined,{numeric:true});if(topic==='agentList'||topic==='records')return Number(a.order||0)-Number(b.order||0)||a.id.localeCompare(b.id);if(topic!=='inbox')return a.id.localeCompare(b.id);var at=function(r){return Number(r.createdAt||(r.message&&r.message.createdAtMs)||r.messageId||0)};return Number(b.rowType==='counts')-Number(a.rowType==='counts')||at(b)-at(a)||Number(b.messageId||0)-Number(a.messageId||0)||a.id.localeCompare(b.id)}); }
  function selector(options) { if(options.topic==='files')return '|files:'+JSON.stringify([options.scope||'team',options.team||null,options.storyId||null,options.agentId||null,options.dir||null]).slice(0,-1); return options.collection !== undefined ? ':'+options.collection : options.order !== undefined ? '|'+options.order : ''; }
  function stateKey(topic, options) { return JSON.stringify([topic, selector(Object.assign({topic:topic},options || {}))]); }
  function matchesView(state, viewKey, resync) {
    if (!viewKey) return true;
    if (!resync && state.viewKey) return state.viewKey === viewKey;
    var suffix = selector(state);
    return suffix ? (state.topic === 'files' ? viewKey.includes(suffix+',') : viewKey.endsWith(suffix)) : !viewKey.includes('|') && !viewKey.startsWith('records:');
  }
  function notify(state, delta) {
    if (!state || !state.value) return;
    state.notified = true;
    state.listeners.slice().forEach(function (callback) { callback(state.value, delta); });
  }
  function sync(topic, state, initial) {
    if (state.value && !state.stale) return Promise.resolve(state.value);
    if ((!active() && !initial) || state.loading || !state.listeners.length) return state.loading || Promise.resolve(state.value);
    state.retry = false;
    state.requested = true;
    var generation = state.generation;
    state.loading = c.invoke('subscribe', { topic: topic, sinceRev: state.value ? state.value.rev : state.sinceRev, window:state.window, collection:state.collection, order:state.order, scope:state.scope, team:state.team, storyId:state.storyId, agentId:state.agentId, dir:state.dir }).then(function (snapshot) {
      state.loading = null;
      if (state.retired) return snapshot;
      if (generation !== state.generation) return sync(topic, state);
      if (!active()) { state.stale = true; return snapshot; }
      state.viewKey = snapshot.viewKey;
      var previous = state.value;
      if (!previous || previous.viewKey !== snapshot.viewKey || previous.rev <= snapshot.rev) state.value = snapshot;
      state.stale = state.retry || !!(state.expected && state.expected.viewKey === state.value.viewKey && state.value.rev < state.expected.rev);
      if (state.stale) return sync(topic, state);
      if (!previous || previous.rev !== state.value.rev || previous.viewKey !== state.value.viewKey || !state.notified || state.moved) { state.moved = false; notify(state, null); }
      return state.value;
    }, function (error) { state.loading = null; if(generation !== state.generation && !state.retired)return sync(topic,state); state.error = error; throw error; });
    return state.loading;
  }
  c.subscribe = function (topic, options) {
    options = options || {};
    var key = stateKey(topic, options);
    var state = topics.get(key);
    if (!state) {
      var first = window.__codesenseFirstPaint;
      state = { generation:0, topic: topic, scope:options.scope, team:options.team, storyId:options.storyId, agentId:options.agentId, dir:options.dir, collection: options.collection, order: options.order, value: first && first.topic === topic && matchesView(options,first.viewKey,true) ? first : null, listeners: [], stale: true, loading: null, sinceRev: options.sinceRev,window:options.window || ((topic === "records" || topic === "files" || topic === "stories") ? [0,200] : undefined) };
      state.viewKey = state.value && state.value.viewKey;
      topics.set(key, state);
    }
    var movedWindow = options.window && JSON.stringify(state.window)!==JSON.stringify(options.window);
    if(movedWindow){state.window=options.window;state.stale=true;state.retry=true;state.notified=false;state.moved=true;}
    var callback = options.onChange || state.noop || (state.noop=function () {});
    if(!state.listeners.includes(callback))state.listeners.push(callback);
    if (state.value && active() && !state.moved) { callback(state.value, null); state.notified = true; }
    var wait = visibilityKnown && (active() || !state.requested) ? sync(topic, state, !state.requested) : new Promise(function (resolve, reject) {
      var off = c.onVisibility(function (shown) {
        if (shown || liveBadge() || !state.requested) { queueMicrotask(function () { off(); sync(topic, state, !state.requested).then(resolve, reject); }); }
      });
    });
    return wait;
  };
  c.unsubscribe = function (topic, callback, options) {
    var retired = [];
    for (var pair of topics) {
      var state = pair[1];
      if(state.topic !== topic || (options && selector(Object.assign({topic:topic},options)) !== selector(state))) continue;
      if (callback) state.listeners = state.listeners.filter(function (cb) { return cb !== callback; });
      else state.listeners = [];
      if (!state.listeners.length) { state.retired = true; state.generation++; topics.delete(pair[0]); if (active()) retired.push(c.invoke('unsubscribe', {topic:topic,collection:state.collection, order:state.order, scope:state.scope, team:state.team, storyId:state.storyId, agentId:state.agentId, dir:state.dir}).catch(function(){})); }
    }
    return Promise.all(retired).then(function(){});
  };
  c.page=function(topic,cursor,options){
    var state = topics.get(stateKey(topic,options));
    if(!state && topic === 'records') {
      var matches=Array.from(topics.values()).filter(function(s){return s.topic===topic;});
      if(matches.length===1)state=matches[0];
    }
    if(!state)return Promise.reject(new Error('subscription unavailable: give collection'));
    return c.invoke('page',{topic:topic,cursor:cursor,collection:state.collection, order:state.order, scope:state.scope, team:state.team, storyId:state.storyId, agentId:state.agentId, dir:state.dir}).then(function(page){
      if(state.value){
        var current=state.value;
        var rows=new Map(page.snapshot.concat(current.snapshot).map(function(row){return [rowId(row),row];}));
        if((topic==='records'||topic==='files'||topic==='stories') && state.window) state.window=[state.window[0],state.window[1]+page.snapshot.length];
        state.value=Object.assign({},page.rev>current.rev?page:current,{snapshot:orderedRows(topic,Array.from(rows.values())),cursor:page.cursor});
        notify(state,null);
      }
      return page;
    });
  };
  c.setUiState = function (topic, patch, requestId) { return c.invoke('setUiState', {topic:topic,patch:patch,requestId:requestId}); };
  var inboxChanged = c.onInboxChanged;
  c.onSessions = function (callback) {
    var active = true, request = 0;
    function changed() {
      var current = ++request;
      c.sessions().then(function (rows) {
        if (active && current === request) callback(rows, null);
      }, function (error) { if (active && current === request) callback(null, error); });
    }
    var offInbox = inboxChanged(changed);
    c.subscribe('sessions',{onChange:changed}).catch(function(error){callback(null,error);});
    return function(){active=false;offInbox();c.unsubscribe('sessions',changed);};
  };
  c.onAgentsChanged = function (callback) {
    function changed() { callback(); }
    c.subscribe('agentDefinitions', {onChange:changed}).catch(function(){});
    var offSessions = c.onSessions(function (_rows, error) { if (!error) callback(); });
    return function () { offSessions(); c.unsubscribe('agentDefinitions', changed); };
  };
  ['onInboxChanged'].forEach(function (name) {
    var legacy = c[name];
    c[name] = function (callback) {
      var offPing = legacy(callback);
      var offSessions = c.onSessions(function (_rows, error) { if (!error) callback(); });
      return function () { offPing(); offSessions(); };
    };
  });
  c.sessions = function () {
    return c.invoke('sessions');
  };
  c.onVisibility(function (shown) {
    visibilityKnown = true; visible = shown;
    for (var pair of topics) {
      if (!shown && !liveBadge()) pair[1].stale = true;
      else if (pair[1].stale) sync(pair[1].topic,pair[1]).catch(function(){});
    }
  });
  window.addEventListener('message', function (event) {
    if (event.source !== window.parent || !event.data) return;
    var token = window.__codesenseProxy && window.__codesenseProxy.token;
    if (!token || event.data.token !== token) return;
    if (parentOrigin !== undefined && event.origin !== parentOrigin) return;
    parentOrigin = event.origin;
    var data = event.data;
    if (data.type === 'codesense:slot-context') { badgeLive = !!(data.declaration && data.declaration.badge === 'live'); return; }
    if (data.type === 'codesense:rebind') window.__codesenseFirstPaint = null;
    if (data.type === 'codesense:stale') {
      for (var pair of topics) pair[1].stale = true;
      return;
    }
    if (!active()) return;
    for (var pair of topics) {
    var state = pair[1];
    if (state.topic !== data.topic) continue;
    if (!matchesView(state, data.viewKey, data.type === 'codesense:resync')) continue;
    if (data.type === 'codesense:resync') { if(state.viewKey !== data.viewKey){state.generation++;state.viewKey=data.viewKey;state.value=null;state.sinceRev=undefined;state.expected=null;state.notified=false;} state.retry = true; state.stale = true; sync(data.topic,state).catch(function(){}); continue; }
    if (data.type !== 'codesense:delta') continue;
    var current = state.value;
    if (current && data.rev <= current.rev) continue;
    if (!state.expected || state.expected.viewKey !== data.viewKey || state.expected.rev < data.rev) state.expected = {viewKey:data.viewKey,rev:data.rev};
    if (!current || current.viewKey !== data.viewKey || (!state.window && data.topic!=="inbox" && (data.baseRev == null ? data.rev - 1 : data.baseRev) !== current.rev)) {
      state.stale = true; sync(data.topic,state).catch(function(){}); continue;
    }
    var rows = new Map(current.snapshot.map(function (row) { return [rowId(row),row]; }));
    data.removes.forEach(function (id) { rows.delete(id); });
    data.upserts.forEach(function (row) { rows.set(rowId(row),row); });
    state.value = {topic:data.topic,viewKey:data.viewKey,rev:data.rev,snapshot:orderedRows(data.topic,Array.from(rows.values())),total:data.total,cursor:(data.topic==='records'||data.topic==='files'||data.topic==='stories') ? (data.cursor || undefined) : current.cursor};
    notify(state,data);
    }
  });
})();
