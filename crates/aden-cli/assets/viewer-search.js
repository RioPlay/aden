// Copyright (c) 2026 RioPlay. SPDX-License-Identifier: AGPL-3.0-or-later
// One ranking contract for both viewers; cache immutable symbol text once.
const AdenSearch = (() => {
  const cache = new WeakMap();
  function entry(node, name) {
    if (!cache.has(node)) cache.set(node, {
      name: name.toLowerCase(), anchor: (node.anchor || '').toLowerCase(),
      text: [name,node.label,node.anchor,node.file,node.group].filter(Boolean).join(' ').toLowerCase()
    });
    return cache.get(node);
  }
  function rank(node,name,query) {
    const item = entry(node,name);
    return item.name === query || item.anchor === query ? 0 : item.name.startsWith(query) ? 1 : item.name.includes(query) ? 2 : 3;
  }
  // Stable rank buckets avoid sorting every matching symbol. With a small
  // result limit, retain only the best entries in each rank, including ties.
  function select(nodes,name,query,limit=Infinity,compare=null,accept=null) {
    const buckets = [[],[],[],[]];
    for (const node of nodes) {
      const label = name(node);
      if (!entry(node,label).text.includes(query) || (accept && !accept(node))) continue;
      const bucket = buckets[rank(node,label,query)];
      if (!compare) { if (bucket.length < limit) bucket.push(node); continue; }
      let lo = 0, hi = bucket.length;
      while (lo < hi) {
        const mid = (lo + hi) >>> 1;
        if (compare(node,bucket[mid]) < 0) hi = mid; else lo = mid + 1;
      }
      if (lo < limit) { bucket.splice(lo,0,node); if (bucket.length > limit) bucket.pop(); }
    }
    return buckets.flat().slice(0,limit);
  }
  return {
    text: (node,name) => entry(node,name).text,
    rank, select
  };
})();
