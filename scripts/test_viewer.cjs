// Run with: node --test scripts/test_viewer.cjs
// Exercise the actual offline template handlers with a small DOM/graph harness.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const html = fs.readFileSync(path.join(__dirname, '../crates/aden-cli/assets/view.html'), 'utf8');

function section(start, end) {
  const from = html.indexOf(start), to = html.indexOf(end, from);
  assert.ok(from >= 0 && to > from, `Missing template section: ${start}`);
  return html.slice(from, to);
}

function template3dSection(start,end) {
  const source=fs.readFileSync(path.join(__dirname,'../crates/aden-cli/assets/view3d.html'),'utf8');
  return source.slice(source.indexOf(start),source.indexOf(end,source.indexOf(start)));
}
function decodeAttr(value) {
  return value.replace(/&quot;/g,'"').replace(/&lt;/g,'<').replace(/&gt;/g,'>').replace(/&amp;/g,'&');
}
test('3D legend and neighbor handlers treat quoted names and IDs as literal data',()=>{
  const elements=new Map();const get=id=>{if(!elements.has(id))elements.set(id,{classList:{add(){}},innerHTML:''});return elements.get(id);};
  const malicious=`x');globalThis.injected=1;//" onfocus="injected=2 &quot; < >`;
  const calls=[];
  const ctx=vm.createContext({document:{getElementById:get},groupIdx:{[malicious]:0},palette:['#abcdef'],
    hexA:()=>'',editorUrl:()=>null,shortName:n=>n.id,regionOf3d:()=>malicious,
    focusById:id=>calls.push(['focus',id]),previewNb:id=>calls.push(['preview',id]),jumpToRegion3d:r=>calls.push(['region',r])});
  vm.runInContext(template3dSection('function esc(s)','function shortName('),ctx);
  vm.runInContext(template3dSection('function buildLegend3d()','function jumpToRegion3d('),ctx);
  vm.runInContext('buildLegend3d()',ctx);
  const legend=get('rl-body').innerHTML;
  assert.equal(decodeAttr(legend.match(/onclick="([^"]*)"/)[1]),`jumpToRegion3d(${JSON.stringify(malicious)})`);
  assert.equal(decodeAttr(legend.match(/class="rl-label" title="([^"]*)"/)[1]),malicious);
  vm.runInContext(decodeAttr(legend.match(/onclick="([^"]*)"/)[1]),ctx);
  vm.runInContext('let lastPreviewId,panelNbs,nbSel;',ctx);
  vm.runInContext(template3dSection('function showPanel(n)','function focusById('),ctx);
  ctx.node={id:'root',neighbors:[{id:malicious}]};
  vm.runInContext('showPanel(node)',ctx);
  const panel=get('p-body').innerHTML;
  for(const attr of ['onclick','onmouseenter']) vm.runInContext(decodeAttr(panel.match(new RegExp(attr+'="([^"]*)"'))[1]),ctx);
  assert.deepEqual(calls,[['region',malicious],['focus',malicious],['preview',malicious]]);
  assert.equal(ctx.injected,undefined);
  assert.ok(!Array.from(panel.matchAll(/<[^>]*>/g),m=>m[0]).some(tag=>tag.includes(' onfocus="')));
  for(const id of [42,'&quot;']) {
    ctx.node={id:'root',neighbors:[{id}]};vm.runInContext('showPanel(node)',ctx);
    const markup=get('p-body').innerHTML;
    for(const attr of ['onclick','onmouseenter']) vm.runInContext(decodeAttr(markup.match(new RegExp(attr+'="([^"]*)"'))[1]),ctx);
    assert.deepEqual(calls.slice(-2),[['focus',String(id)],['preview',String(id)]]);
  }
});

test('3D bootstrap catches renderer failure without timers and preserves successful inline handlers',()=>{
  const source=fs.readFileSync(path.join(__dirname,'../crates/aden-cli/assets/view3d.html'),'utf8');
  const script=source.slice(source.indexOf('const DATA ='),source.lastIndexOf('</script>')).replace('/*ADEN_DATA*/',JSON.stringify({nodes:[],edges:[]}));
  function run(fail,sibling='') {
    const elements=new Map();let timers=0,focused;
    function element(id='') {return {id,style:{},children:[],classList:{add(){},remove(){},contains(){return false;},toggle(){}},
      setAttribute(k,v){this[k]=v;},append(...items){this.children.push(...items);},replaceChildren(){this.children=[];},
      focus(){focused=this;},addEventListener(){},querySelectorAll(){return[];},offsetHeight:0};}
    const get=id=>{if(!elements.has(id))elements.set(id,element(id));return elements.get(id);};
    let graph;graph=new Proxy(()=>{}, {get(_,key){if(key==='then')return undefined;if(key==='controls')return ()=>({target:{x:0,y:0,z:0},addEventListener(){}});return ()=>graph;}});
    const window={matchMedia:()=>({matches:true}),addEventListener(){}};
    const context=vm.createContext({window,document:{body:element('body'),getElementById:get,createElement:()=>element(),addEventListener(){},querySelectorAll:()=>[]},
      URL,location:{href:'file:///tmp/view3d.html',hash:''},localStorage:{getItem(){return null;},setItem(){}},innerWidth:1000,innerHeight:800,addEventListener(){},
      requestAnimationFrame(){timers++;},setTimeout(){timers++;},setInterval(){timers++;},clearTimeout(){},clearInterval(){},
      ForceGraph3D:()=>()=>{if(fail)throw Error('private renderer details');return graph;}});
    vm.runInContext(script.replace('/*SIBLING*/',sibling),context);
    return {context,window,get,timers,focused};
  }
  for(const sibling of ['', 'view.html', 'javascript:alert(1)']) {
    const failed=run(true,sibling);assert.equal(failed.timers,0);
    assert.equal(failed.focused.role,'alert');
    const children=failed.focused.children;
    assert.ok(children.some(n=>n.textContent==='3D view unavailable'));
    assert.ok(!JSON.stringify(children).includes('private renderer details'));
    assert.equal(children.some(n=>n.textContent==='Open 2D view'),sibling==='view.html');
  }
  const success=run(false);
  for(const name of ['dismissHoverCard3d','toggleLegend','jumpToRegion3d','focusById','previewNb','pickResult']) assert.equal(typeof success.window[name],'function',name);
  success.window.dismissHoverCard3d();success.window.toggleLegend();success.window.jumpToRegion3d('missing');
  success.window.focusById('missing');success.window.previewNb('missing');success.window.pickResult(0);
});

test('search caches global/drill identities and preserves rank, degree, exclusions and expansion', () => {
  const entries=Array.from({length:200},(_,i)=>({a:'aden://module/demo#'+(i%3?'render_'+i:'render'),n:i%3?'render_'+i:'render',g:'demo',k:'fn',d:i%11}));
  const visible={id:'visible',anchor:entries[0].a,label:entries[0].n,deg:50};
  const ctx=vm.createContext({DATA:{all_anchors:entries,drill:{c:{nodes:[{id:'a',anchor:'aden://module/demo#render_a'}]}}},
    hasDrill:true,byId:{visible},shortName:n=>(n.anchor||n.label||n.id).split('#').pop()});
  vm.runInContext(fs.readFileSync(path.join(__dirname,'../crates/aden-cli/assets/viewer-search.js'),'utf8'),ctx);
  vm.runInContext(section('const drillSearchCandidates','function resultShape('),ctx);
  const first=vm.runInContext("allAnchorCandidates('render',new Set())",ctx);
  const second=vm.runInContext("allAnchorCandidates('render',new Set())",ctx);
  assert.equal(first[0],second[0]);
  for (const query of ['render','render_1','demo','missing']) {
    ctx.query=query;
    const actual=vm.runInContext("allAnchorCandidates(query,new Set(['aden://module/demo#render']))",ctx);
    const expected=entries.filter(e=>(e.n+' '+e.a+' '+e.g).toLowerCase().includes(query))
      .map(e=>({_global:true,id:e.a,anchor:e.a,label:e.n,group:e.g,kind:e.k,deg:e.d||0}))
      .filter(e=>e.anchor!=='aden://module/demo#render');
    const rank=n=>{const name=n.anchor.split('#').pop().toLowerCase();return name===query||n.anchor.toLowerCase()===query?0:name.startsWith(query)?1:name.includes(query)?2:3;};
    expected.sort((a,b)=>rank(a)-rank(b)||b.deg-a.deg);
    assert.deepEqual(Array.from(actual,n=>n.id),expected.slice(0,6).map(n=>n.id));
  }
  const drill=vm.runInContext("inViewCandidates().find(n=>n.id==='c:a')",ctx);
  assert.equal(drill,vm.runInContext("inViewCandidates().find(n=>n.id==='c:a')",ctx));
  const expanded={...drill,deg:99};ctx.byId['c:a']=expanded;
  assert.equal(expanded,vm.runInContext("inViewCandidates().find(n=>n.id==='c:a')",ctx));
  delete ctx.byId['c:a'];
  assert.equal(drill,vm.runInContext("inViewCandidates().find(n=>n.id==='c:a')",ctx));
});

test('envelope geometry is reused only while visible members, coordinates and source mass are unchanged', () => {
  let render, hullCalls=0;
  const nodes=[{id:'a',group:'g',x:0,y:0,loc:10},{id:'b',group:'g',x:100,y:0,loc:20},{id:'c',group:'g',x:50,y:100,loc:30}];
  const hidden=new Set(), byId=Object.fromEntries(nodes.map(n=>[n.id,n]));
  const ctx=vm.createContext({Graph:{onRenderFramePre(fn){render=fn;}},byId,
    envelopes:true,minimal:false,idVisible:id=>!hidden.has(id),palette:['#abcdef'],groupIdx:{g:0},
    groupPulse:new Map(),REDUCED_MOTION:true,hexA:(c,a)=>c+':'+a, countHull:()=>hullCalls++});
  vm.runInContext(section('function hullOf(', 'let fitPending ='),ctx);
  vm.runInContext('const rawHull=hullOf; hullOf=points=>{countHull();return rawHull(points);};',ctx);
  function draw(scale=1) {
    const ops=[];
    const canvas={createRadialGradient(...a){ops.push(['gradient',...a]);return {addColorStop(...a){ops.push(['stop',...a]);}};},
      beginPath(){ops.push(['begin']);},arc(...a){ops.push(['arc',...a]);},fill(){ops.push(['fill',String(this.fillStyle)]);},
      moveTo(...a){ops.push(['move',...a]);},quadraticCurveTo(...a){ops.push(['curve',...a]);},closePath(){ops.push(['close']);},
      stroke(){ops.push(['stroke',this.strokeStyle,this.lineWidth]);},fillText(...a){ops.push(['text',...a,this.font]);}};
    render(canvas,scale);return ops;
  }
  const first=draw();
  for(let i=0;i<20;i++) assert.deepEqual(draw(),first);
  assert.equal(hullCalls,1,'static frames must not rebuild convex hull');
  nodes[0].x=5;assert.notDeepEqual(draw(),first);assert.equal(hullCalls,2,'physics movement');
  nodes[0].y=10;draw();assert.equal(hullCalls,3,'drag movement');
  hidden.add('c');assert.equal(draw().some(o=>o[0]==='curve'),false);assert.equal(hullCalls,3,'two-node group only has nebula');
  hidden.delete('c');draw();assert.equal(hullCalls,4,'visibility restoration');
  nodes[0].loc=100;assert.ok(draw().some(o=>o[0]==='text'&&o[1].includes('150 loc')));assert.equal(hullCalls,5);
  byId.a={...nodes[0]};draw();assert.equal(hullCalls,6,'same-ID graph replacement');
  ctx.groupPulse.set('g',1);assert.notDeepEqual(draw(),first);assert.equal(hullCalls,6,'dynamic glow does not invalidate geometry');
  const scaled=draw(2);assert.ok(scaled.some(o=>o[0]==='stroke'&&o[2]===1.5));assert.equal(hullCalls,6,'zoom styling remains dynamic');
  hidden.add('b');hidden.add('c');assert.equal(draw().length,0,'single node has no envelope');
  hidden.add('a');draw();assert.equal(vm.runInContext('envelopeShapes.size',ctx),0,'hidden groups release cached geometry');
});

test('3D hidden wraps skip scene/geometry work and refresh before becoming visible', () => {
  const template=fs.readFileSync(path.join(__dirname,'../crates/aden-cli/assets/view3d.html'),'utf8');
  let traversals=0;
  const ctx=vm.createContext({Graph:{scene(){return {traverse(){traversals++;}};}},nodes:[],setTimeout(){},setInterval(){}});
  vm.runInContext(template.slice(template.indexOf('let bubblesOn = true;'),template.indexOf('// The focused orb breathes')),ctx);
  vm.runInContext('bubblesOn=false; updateBubbles();',ctx);assert.equal(traversals,0);
  vm.runInContext('toggleBubbles()',ctx);assert.equal(traversals,1,'enable immediately refreshes rather than waiting for interval');
  vm.runInContext('toggleBubbles();updateBubbles()',ctx);assert.equal(traversals,1);
});

test('startup preserves a deep link instead of recording an empty overview', () => {
  const hash='#a=aden%3A%2F%2Fmodule%2Fdemo%23render';
  const location={hash};
  let restored=null;
  const ctx=vm.createContext({location,vmode:'normal',hasDrill:false,
    showOverview(record=true) {if(record) location.hash='';},
    applyVerbosity() {},setTimeout(fn) {fn();},restoreFromHash(value) {restored=value;}});
  vm.runInContext(section('// ── init ──','</script>'),ctx);
  assert.equal(location.hash,hash);
  assert.equal(restored,hash.slice(1));
});

test('restoring browser history never pushes another entry or decodes an anchor twice', () => {
  const calls=[];
  const ctx=vm.createContext({URLSearchParams,location:{pathname:'/',search:'',hash:''},
    history:{pushState:()=>calls.push('push')},document:{getElementById:()=>null},window:{addEventListener(){}},
    byId:{one:{id:'one',anchor:'aden://module/p#percent%name'}},goNode:n=>calls.push(n.anchor),
    showOverview:()=>vm.runInContext("updateHash('')",ctx)});
  vm.runInContext(section('let restoringHash = false;', '// ── activity replay'),ctx);
  vm.runInContext("restoreFromHash('');restoreFromHash('a=aden%3A%2F%2Fmodule%2Fp%23percent%25name')",ctx);
  assert.deepEqual(calls,['aden://module/p#percent%name']);
});

function harness() {
  const elements = new Map();
  function element(id) {
    if (!elements.has(id)) {
      const classes = new Set();
      elements.set(id, {
        id, style: {}, textContent: '', value: '', dataset: {},
        classList: {
          contains: c => classes.has(c), add: c => classes.add(c), remove: c => classes.delete(c),
          toggle(c, on = !classes.has(c)) { on ? classes.add(c) : classes.delete(c); },
        },
        scrollIntoView() {}, addEventListener() {}, matches: () => false,
      });
    }
    return elements.get(id);
  }
  const calls = [];
  const document = {
    activeElement: { matches: () => false }, body: element('body'),
    getElementById: id => id.startsWith('nb-') ? elements.get(id) : element(id),
    querySelectorAll: selector => selector === '.show-more' ? [] : [...elements.values()].filter(e => e.id.startsWith('nb-')),
  };
  let handler;
  const Graph = {
    zoom: (v) => v === undefined ? 1 : calls.push(['zoom', v]),
    centerAt: (x, y) => x === undefined ? { x: 0, y: 0 } : calls.push(['pan', x, y]),
    nodeVisibility() {}, linkVisibility() {},
  };
  const context = vm.createContext({
    document, Graph, Date, Set, Map, console,
    addEventListener: (_, fn) => { handler = fn; },
    byId: {}, curLinks: [], growMode: false, revealed: new Set(), pivotSet: null,
    densityVisible: null, densityFrac: 1, depthLimit: null, highlightLinks: new Set(), activeTypes: new Set(),
    panelCur: null, hoverNode: null, panelNbs: [], nbSel: -1,
    qEl: { focus: () => calls.push(['search']) }, resEl: element('results'), matches: [],
    touring: false, dur: n => n,
    fitAll: () => calls.push(['fit']), stopTour() {}, stopPlay() {},
    showAllSymbols: () => calls.push(['all']), toggleLabels: () => calls.push(['labels']),
    previewNode: id => calls.push(['preview', id]),
    focusNode: id => calls.push(['follow', id]), panelBack: () => calls.push(['back']),
  });
  vm.runInContext(section('// ── keyboard ', '// ── overUI detection'), context);
  function key(key, extra = {}) {
    let prevented = false;
    handler({ key, preventDefault() { prevented = true; }, ...extra });
    return prevented;
  }
  return { context, calls, key, element, document, elements };
}

test('Vim canvas motions, prefixes, zoom, and panel follow/back', () => {
  const h = harness();
  h.key('h'); h.key('j'); h.key('k'); h.key('l');
  assert.deepEqual(h.calls.splice(0), [['pan', -80, 0], ['pan', 0, 80], ['pan', 0, -80], ['pan', 80, 0]]);
  h.key('g'); h.key('g'); h.key('g'); h.key('a'); h.key('+'); h.key('L');
  assert.deepEqual(h.calls.splice(0), [['fit'], ['all'], ['zoom', 1.25], ['labels']]);
  h.element('panel').classList.add('open');
  h.context.panelNbs = ['a', 'b'];
  h.key('j'); h.key('j'); h.key('l'); h.key('h');
  assert.deepEqual(h.calls, [['preview', 'a'], ['preview', 'b'], ['follow', 'b'], ['back']]);
});

test('typing, modifiers, and native button activation are never hijacked', () => {
  const h = harness();
  h.document.activeElement.matches = selector => selector.includes('input');
  for (const key of ['h', 'j', '/', ' ', '+']) assert.equal(h.key(key), false);
  h.document.activeElement.matches = selector => selector.includes('button');
  assert.equal(h.key('Enter'), false);
  assert.equal(h.key(' '), false);
  assert.equal(h.key('l', { ctrlKey: true }), false);
  assert.deepEqual(h.calls, []);
});

test('relation navigation expands collapsed rows and reaches beyond row 80', () => {
  const h = harness();
  h.element('panel').classList.add('open');
  h.context.panelNbs = Array.from({ length: 120 }, (_, i) => `symbol-${i}`);
  let expanded = false;
  const more = { dataset: { start: '6', ids: JSON.stringify(h.context.panelNbs.slice(6)) }, click() {
    expanded = true; h.element('nb-119');
  } };
  h.document.querySelectorAll = selector => selector === '.show-more' ? [more] : [];
  h.key('G'); h.key('Enter');
  assert.equal(expanded, true);
  assert.deepEqual(h.calls, [['preview', 'symbol-119'], ['follow', 'symbol-119']]);
});

test('density readout reports actual visible nodes and full density restores them', () => {
  const h = harness();
  vm.runInContext(section('// ── visibility + density + depth', '// ── replay '), h.context);
  h.context.byId = Object.fromEntries(Array.from({ length: 10 }, (_, i) => [`n${i}`, { id: `n${i}`, deg: i }]));
  vm.runInContext('setDensity(20)', h.context);
  assert.equal(h.element('density-count').textContent, '2 / 10 visible');
  vm.runInContext('setDensity(100)', h.context);
  assert.equal(h.element('density-count').textContent, '10 / 10 visible');
});

test('region buttons preserve quoted names in executable HTML attributes', () => {
  const h = harness();
  const name = 'crate "quoted" & <tag>';
  h.context.groupIdx = { [name]: 0 };
  h.context.palette = ['#89b4fa'];
  h.context.hexA = () => 'rgba(0,0,0,.4)';
  h.context.jumpToRegion = value => h.calls.push(value);
  for (const fn of ['esc', 'escAttr']) {
    vm.runInContext(html.match(new RegExp(`^function ${fn}\\(.*$`, 'm'))[0], h.context);
  }
  vm.runInContext(section('function buildLegend()', 'function jumpToRegion('), h.context);
  vm.runInContext('buildLegend()', h.context);
  const markup = h.element('rl-body').innerHTML;
  const handler = markup.match(/onclick="([^"]*)"/)[1]
    .replaceAll('&quot;', '"').replaceAll('&lt;', '<').replaceAll('&gt;', '>').replaceAll('&amp;', '&');
  vm.runInContext(handler, h.context);
  assert.deepEqual(h.calls, [name]);
});

test('graph panel context preserves edge direction and prioritizes the selected relationship', async () => {
  const root = {id:'root',anchor:"aden://module/demo#it's_render",file:'src/render.rs',line:18,snippet:'fn render() {}'};
  const peers = Array.from({length:40},(_,i)=>({id:`n${i}`,anchor:`aden://module/demo#peer${i}`}));
  const byId = Object.fromEntries([root,...peers].map(n=>[n.id,n]));
  let copied;
  const ctx = vm.createContext({panelContextNode:root,byId,level:'overview',nbSel:39,panelNbs:peers.map(n=>n.id),
    curLinks:peers.map((n,i)=>i%2?{source:n,target:root,type:'Calls'}:{source:root.id,target:n.id,type:'Uses'}),
    idOf:value=>typeof value==='object'?value.id:value,DATA:{mode:'blast'},button:{},
    navigator:{clipboard:{writeText:async text=>{copied=text;}}}});
  vm.runInContext(fs.readFileSync(path.join(__dirname,'../crates/aden-cli/assets/viewer-context.js'),'utf8'),ctx);
  vm.runInContext(section('async function copyAnchor(', 'function searchAndGo('),ctx);
  await vm.runInContext('copyPanelContext(button)',ctx);
  assert.ok(copied.includes(root.anchor));
  assert.ok(copied.includes('Source: src/render.rs:18'));
  assert.ok(copied.includes(root.snippet));
  assert.match(copied,/Relationships: 24 copied of 40/);
  assert.ok(copied.includes(`* Selected: aden://module/demo#peer39 --Calls--> ${root.anchor}`));
  assert.ok(copied.includes(`- ${root.anchor} --Uses--> aden://module/demo#peer0`));
  assert.match(copied,/including collapsed rows/);
  assert.equal(ctx.button.textContent,'Copied context');
});

test('panel copy controls are native buttons and never embed an anchor in executable code', () => {
  const h = harness();
  const anchor = `aden://module/quoted\"#it's_<render>`;
  Object.assign(h.context,{lastPreviewId:null,panelContextNode:null,panelStack:[],
    n:{id:'one',anchor},row:()=>'',editorUrl:()=>'',breadcrumb:()=>'',
    groupedNeighbours:()=>[],shortName:n=>n.anchor});
  for (const fn of ['esc','escAttr']) vm.runInContext(html.match(new RegExp(`^function ${fn}\\(.*$`,'m'))[0],h.context);
  vm.runInContext(section('function showPanel(', 'function showMoreGroup('),h.context);
  vm.runInContext('showPanel(n)',h.context);
  const markup = h.element('p-body').innerHTML;
  assert.ok(markup.includes('title="aden://module/quoted&quot;#it\'s_&lt;render&gt;"'));
  assert.match(markup,/<button type="button" class="copy-btn" onclick="copyPanelContext\(this\)">Copy context<\/button>/);
  assert.match(markup,/copyAnchor\(document.getElementById\('panel-anchor'\).textContent, this\)/);
  assert.equal(h.context.panelContextNode,h.context.n);
});
