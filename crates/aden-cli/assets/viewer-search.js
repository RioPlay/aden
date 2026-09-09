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
  return {
    text: (node,name) => entry(node,name).text,
    rank(node,name,query) {
      const item = entry(node,name);
      return item.name === query || item.anchor === query ? 0 : item.name.startsWith(query) ? 1 : item.name.includes(query) ? 2 : 3;
    }
  };
})();
