// Run with: node --test scripts/test_simple_viewer.cjs
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const html = fs.readFileSync(path.join(__dirname, '../crates/aden-cli/assets/view-simple.html'), 'utf8');
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
