import assert from 'node:assert/strict';
import path from 'node:path';
import { test } from 'node:test';
import { resolveLspCommand } from '../src/serverCommand.ts';

test('default and explicitly configured commands stay on PATH', () => {
    assert.equal(resolveLspCommand(undefined, '/workspace'), 'aden-lsp');
    assert.equal(resolveLspCommand('aden-lsp', '/workspace'), 'aden-lsp');
    assert.equal(resolveLspCommand(' custom-server ', '/workspace'), 'custom-server');
    assert.equal(resolveLspCommand(' ', undefined), 'aden-lsp');
});

test('explicit filesystem paths preserve absolute paths and resolve relative paths', () => {
    const root = path.resolve('workspace');
    const absolute = path.resolve('tools/aden-lsp');
    assert.equal(resolveLspCommand(absolute, root), absolute);
    assert.equal(resolveLspCommand('./target/debug/aden-lsp', root), path.resolve(root, 'target/debug/aden-lsp'));
});

test('relative configuration without a workspace fails actionably', () => {
    assert.throws(() => resolveLspCommand('./aden-lsp', undefined), /requires an open workspace folder/);
});
