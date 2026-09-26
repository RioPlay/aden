// Copyright (c) 2026 RioPlay. SPDX-License-Identifier: AGPL-3.0-or-later
// The same bounded, plain-text context packet and clipboard fallback in both viewers.
const AdenContext = (() => {
  const MAX_CHARS = 16000, MAX_RELATIONSHIPS = 24;
  function clip(value, limit) {
    const text = String(value ?? '');
    if (text.length <= limit) return text;
    const suffix = '… [clipped]';
    let end = limit - suffix.length;
    if (/^[\uDC00-\uDFFF]$/.test(text[end])) end--;
    return text.slice(0, end) + suffix;
  }
  const field = (value, limit = 200) => clip(String(value ?? '').replace(/[\r\n\t]+/g, ' '), limit);
  const identity = node => field(node?.anchor || node?.label || node?.id || 'unknown', 600);
  function build({ node, data = {}, relationships = [], totalRelationships = relationships.length, view = '' }) {
    const receipt = data.context_receipt || {};
    const lines = [
      'Aden context',
      `Anchor: ${field(node.anchor || '(not available)', 2048)}`,
      `Source: ${node.file ? field(node.file, 2048) + (node.line ? ':' + field(node.line, 20) : '') : '(not available)'}`,
      `Export mode: ${field(data.mode || 'unknown')}. Relationships outside this export are not included.`,
      `Exported at: ${field(data.generated_at || 'not recorded')}`,
      `Git commit at export: ${field(data.git_hash || 'not recorded')}`,
      `Graph revision at export: ${field(receipt.graph_revision || data.graph_revision || 'not recorded')}`,
      `Freshness at export: ${field(receipt.freshness || data.freshness || 'not recorded')}`,
      'Freshness now: unknown (static export; source and relationships may have changed).',
      `View: ${field(view, 600)}`,
      '',
      node.snippet ? 'Source preview (exported excerpt; completeness unknown):' : 'Source preview: not included in this export.',
    ];
    if (node.snippet) lines.push(clip(node.snippet, 6000));
    const base = lines.join('\n');
    const copied = [];
    let length = base.length;
    for (const edge of relationships.slice(0, MAX_RELATIONSHIPS)) {
      const row = `${edge.selected ? '* Selected: ' : '- '}${identity(edge.from)} --${field(edge.type || 'Related', 80)}--> ${identity(edge.to)}`;
      // Reserve space for counts and the explicit incompleteness note.
      if (length + row.length + 1 > MAX_CHARS - 400) break;
      copied.push(row); length += row.length + 1;
    }
    const total = Math.max(totalRelationships, relationships.length);
    const summary = `Relationships: ${copied.length} copied of ${total} in this view; arrows run from source to target.`;
    const omitted = total > copied.length ? '\nAdditional relationships are omitted by the current page or copy limit.' : '';
    return `${base}\n\n${summary}\n${copied.join('\n')}${omitted}`;
  }
  async function copy(text, button, label = 'context') {
    try {
      await navigator.clipboard.writeText(text);
      button.textContent = `Copied ${label}`;
    } catch {
      // Local-file viewers and denied clipboard permissions still offer the exact packet.
      const dialog = document.createElement('dialog');
      dialog.setAttribute('aria-label', `Copy ${label} manually`);
      dialog.style.cssText = 'box-sizing:border-box;width:min(720px,90vw);max-height:85vh;background:Canvas;color:CanvasText;border:1px solid GrayText;border-radius:8px;padding:20px;';
      const instructions = document.createElement('p');
      instructions.textContent = `Clipboard access is unavailable. Press Ctrl+C (⌘C on Mac) to copy the selected ${label}.`;
      const content = document.createElement('textarea');
      content.value = text; content.readOnly = true;
      content.setAttribute('aria-label', `${label} to copy`);
      content.style.cssText = 'box-sizing:border-box;display:block;width:100%;height:45vh;margin:12px 0;font:12px/1.5 monospace;';
      const close = document.createElement('button');
      close.type = 'button'; close.textContent = 'Close';
      close.addEventListener('click', () => dialog.close());
      dialog.addEventListener('close', () => dialog.remove());
      dialog.append(instructions, content, close);
      document.body.append(dialog); dialog.showModal(); content.focus(); content.select();
      button.textContent = 'Select and copy';
    }
  }
  return { build, copy, MAX_CHARS, MAX_RELATIONSHIPS };
})();
