// Run with: node --test scripts/test_simple_viewer.cjs
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const html = fs.readFileSync(path.join(__dirname, '../crates/aden-cli/assets/view-simple.html'), 'utf8');
const contextHelpers = fs.readFileSync(path.join(__dirname, '../crates/aden-cli/assets/viewer-context.js'), 'utf8');
const context = vm.createContext({});
vm.runInContext(html.slice(html.indexOf('const nameOf'), html.indexOf('const graph =')), context);
const index = data => { context.data = data; return vm.runInContext('indexGraph(data)', context); };

test('shared search ranks exact symbols and anchors ahead of popular partial matches', () => {
  const ctx = vm.createContext({});
  vm.runInContext(fs.readFileSync(path.join(__dirname,'../crates/aden-cli/assets/viewer-search.js'),'utf8'),ctx);
  const result = vm.runInContext(`
    const nodes = [{anchor:'aden://module/p#render_all',label:'render_all'},
      {anchor:'aden://module/p#render',label:'render'},
      {anchor:'aden://module/p#pre_render',label:'pre_render'},
      {anchor:'aden://module/render/file#other',label:'other'}];
    nodes.sort((a,b)=>AdenSearch.rank(a,a.label,'render')-AdenSearch.rank(b,b.label,'render'));
    JSON.stringify({order:nodes.map(n=>n.label),exact:AdenSearch.rank(nodes[0],nodes[0].label,nodes[0].anchor),path:AdenSearch.text(nodes[3],nodes[3].label).includes('render/file')});
  `,ctx);
  assert.deepEqual(JSON.parse(result),{order:['render','render_all','pre_render','other'],exact:0,path:true});
});

test('indexes complete directed adjacency, isolated nodes and dangling edges', () => {
  const nodes = [{id:'root', anchor:'aden://module/demo/src#root'}, {id:'alone', anchor:'aden://module/demo/src#'}];
  const edges = [];
  for (let i=0;i<4005;i++) { nodes.push({id:String(i)}); edges.push({from:'root',to:String(i),type:'Calls'}); }
  edges.push({source:{id:'0'},target:{id:'root'},type:'Uses'}, {from:'missing',to:'root'});
  const graph = index({nodes,edges});
  assert.equal(graph.nodes.size,4007);
  assert.equal(graph.outgoing.get('root').length,4005);
  assert.equal(graph.incoming.get('root').length,1);
  assert.equal(graph.outgoing.get('alone').length,0);
  assert.equal(graph.edges.length,4006);
  assert.equal(graph.anchors.get('aden://module/demo/src#root'),'root');
  assert.equal(vm.runInContext("nameOf({anchor:'aden://module/demo/src#'})",context),'src');
});

test('community members remain navigable with correctly scoped edges', () => {
  const graph = index({nodes:[{id:'c1'},{id:'c2'}],drill:{
    c1:{nodes:[{id:'a'},{id:'b'}],edges:[{from:'a',to:'b',type:'Calls'}]},
    c2:{nodes:[{id:'a'}],edges:[]}
  }});
  assert.equal(graph.nodes.size,5);
  assert.equal(graph.outgoing.get('c1:a')[0].to,'c1:b');
  assert.equal(graph.incoming.get('c2:a')[0].from,'c2');
  assert.equal(graph.outgoing.get('c1').length,2);
});

test('back restores the exact relationship position and filters after following a node', () => {
  const elements = Object.fromEntries(['search','direction','edge-type','structural','symbols'].map(id=>[id,{value:'',checked:false,scrollTop:0}]));
  const saved = {id:'origin',graphPage:3,rowPage:6,rowSelection:152,symbolPage:2,searchSelection:85,
    query:'render',direction:'in',type:'Calls',structural:false,scrollX:0,scrollY:680,symbolsScroll:140};
  const ctx = vm.createContext({$:id=>elements[id],saved});
  vm.runInContext(`
    let current='other',graphPage=0,rowPage=0,rowSelection=-1,symbolPage=0,searchSelection=0;
    let scrollX=0,scrollY=0;
    const historyStack=[saved];
    function searchSymbols() { symbolPage=0; searchSelection=0; }
    function selectNode(id) { current=id; }
    function filterRelations() { graphPage=0;rowPage=0;rowSelection=-1; }
    function renderSymbols() {} function renderDiagram() {} function renderRows() {}
    function scrollTo(x,y) { scrollX=x;scrollY=y; }
  `,ctx);
  vm.runInContext(html.slice(html.indexOf('function captureNavigation()'),html.indexOf('function selectNode(')),ctx);
  vm.runInContext(html.slice(html.indexOf('function restoreNavigation('),html.indexOf('function back()')),ctx);
  vm.runInContext('restoreNavigation(saved)',ctx);
  assert.deepEqual(JSON.parse(vm.runInContext('JSON.stringify(captureNavigation())',ctx)),saved);
  assert.equal(vm.runInContext('current',ctx),'origin');
});

test('copy context includes the current relationship page, source, selection, and export provenance', async () => {
  const anchor = 'aden://module/demo/src#render';
  const root = {id:'root',anchor,file:'C:\\project with spaces\\src\\demo.rs',line:37,snippet:'fn render() {\n    paint();\n}'};
  const peers = Array.from({length:51},(_,i)=>({id:`n${i}`,anchor:`aden://module/demo/src#peer${i}`}));
  const graph = {nodes:new Map([root,...peers].map(n=>[n.id,n]))};
  const elements = {'direction':{value:'out'},'edge-type':{value:'Calls'},'structural':{checked:false},'copy-context':{}};
  let copied;
  const ctx = vm.createContext({graph,current:'root',rowPage:1,ROW_PAGE:24,rowSelection:30,
    relations:peers.map(n=>({from:'root',to:n.id,type:'Calls'})),$:id=>elements[id],
    DATA:{mode:'graph',generated_at:'2026-09-26T12:00:00Z',git_hash:'abc123',context_receipt:{freshness:'current',graph_revision:'rev42'}},
    navigator:{clipboard:{writeText:async text=>{copied=text;}}}});
  vm.runInContext(contextHelpers,ctx);
  vm.runInContext(html.slice(html.indexOf('async function copyContext()'),html.indexOf("$('search').addEventListener('input'")),ctx);
  await vm.runInContext('copyContext()',ctx);
  assert.ok(copied.includes(`Anchor: ${anchor}`));
  assert.ok(copied.includes('Source: C:\\project with spaces\\src\\demo.rs:37'));
  assert.ok(copied.includes(root.snippet));
  assert.match(copied,/Source preview \(exported excerpt; completeness unknown\)/);
  assert.match(copied,/Freshness at export: current/);
  assert.match(copied,/Freshness now: unknown \(static export/);
  assert.match(copied,/Graph revision at export: rev42/);
  assert.match(copied,/Git commit at export: abc123/);
  assert.match(copied,/relationship page 2; direction=out; type=Calls; structural=excluded/);
  assert.match(copied,/Relationships: 24 copied of 51/);
  assert.ok(copied.includes('* Selected: '+anchor+' --Calls--> aden://module/demo/src#peer30'));
  assert.ok(!copied.includes('--> aden://module/demo/src#peer0\n'));
  assert.match(copied,/Additional relationships are omitted/);
  assert.equal(elements['copy-context'].textContent,'Copied context');
  assert.ok(html.includes("$('copy-context').addEventListener('click',copyContext)"));
  assert.ok(html.includes("else if(e.key==='Y') copyContext()"));
});

test('context stays bounded with large source, paths, and relationships and does not invent freshness', () => {
  const ctx = vm.createContext({});
  vm.runInContext(contextHelpers,ctx);
  const packet = vm.runInContext(`AdenContext.build({
    node:{anchor:'aden://'+ 'a'.repeat(9000),file:'x'.repeat(9000),snippet:'😀'.repeat(9000)},
    relationships:Array.from({length:200},(_,i)=>({from:{anchor:'from'+i+'a'.repeat(2000)},to:{anchor:'to'+i+'b'.repeat(2000)},type:'Calls'}))
  })`,ctx);
  assert.ok(packet.length <= vm.runInContext('AdenContext.MAX_CHARS',ctx));
  assert.match(packet,/\[clipped\]/);
  assert.match(packet,/Relationships: \d+ copied of 200/);
  assert.match(packet,/Freshness at export: not recorded/);
  assert.match(packet,/Freshness now: unknown/);
  assert.match(packet,/Additional relationships are omitted/);
  assert.ok(!/[\uD800-\uDBFF](?![\uDC00-\uDFFF])/.test(packet),'clipping preserves surrogate pairs');
});

test('missing and denied clipboard APIs offer an accessible selected fallback without interpreting source as HTML', async () => {
  for (const navigator of [{},{clipboard:{writeText:async()=>{throw new Error('denied');}}}]) {
    const dialogs = [];
    const makeElement = tag => ({tag,style:{},attributes:{},events:{},
      setAttribute(name,value){this.attributes[name]=value;},
      addEventListener(name,fn){this.events[name]=fn;},
      append(...children){this.children=children;},
      showModal(){this.open=true;},focus(){this.focused=true;},select(){this.selected=true;},
      close(){this.open=false;this.events.close();},remove(){this.removed=true;}});
    const document = {createElement:makeElement,body:{append:dialog=>dialogs.push(dialog)}};
    const button = {};
    const text = '<script>unsafe()</script>\n<svg onload="unsafe()">';
    const ctx = vm.createContext({navigator,document,button,text});
    vm.runInContext(contextHelpers,ctx);
    await vm.runInContext('AdenContext.copy(text,button)',ctx);
    assert.equal(dialogs.length,1);
    const dialog = dialogs[0], content = dialog.children[1];
    assert.equal(dialog.attributes['aria-label'],'Copy context manually');
    assert.equal(content.value,text);
    assert.equal(content.readOnly,true);
    assert.equal(content.focused,true);
    assert.equal(content.selected,true);
    assert.equal(content.innerHTML,undefined);
    assert.equal(button.textContent,'Select and copy');
    dialog.children[2].events.click();
    assert.equal(dialog.removed,true);
  }
});
