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
