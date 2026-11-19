import assert from 'node:assert/strict';
import { registerHooks } from 'node:module';
import { test } from 'node:test';

const state = globalThis.adenExtensionTest = {};
const vscode = `
export const ExtensionContext = undefined;
export const workspace = {
    get isTrusted() { return globalThis.adenExtensionTest.trusted; },
    workspaceFolders: [{ uri: { fsPath: process.cwd() } }],
    getConfiguration() { globalThis.adenExtensionTest.configReads++; return { get() { return undefined; } }; },
    onDidGrantWorkspaceTrust(callback) { globalThis.adenExtensionTest.grant = callback; return { dispose() {} }; },
    createFileSystemWatcher() { return { dispose() {} }; }
};
export const window = {
    createOutputChannel() { return { appendLine(s) { globalThis.adenExtensionTest.output = s; }, dispose() {}, show() {} }; },
    async showErrorMessage(message) { globalThis.adenExtensionTest.error = message; }
};
`;
const languageclient = `
export const LanguageClientOptions = undefined, ServerOptions = undefined;
export const TransportKind = { stdio: 0 };
export class LanguageClient {
    constructor(id, name, options) { globalThis.adenExtensionTest.command = options.command; }
    async start() { globalThis.adenExtensionTest.starts++; if (globalThis.adenExtensionTest.fail) throw new Error('ENOENT'); }
    async stop() {}
}
`;
registerHooks({
    resolve(specifier, context, nextResolve) {
        if (specifier === 'vscode' || specifier === 'vscode-languageclient/node') {
            const source = specifier === 'vscode' ? vscode : languageclient;
            return { url: 'data:text/javascript,' + encodeURIComponent(source), shortCircuit: true };
        }
        if (specifier === './serverCommand') {
            return nextResolve('./serverCommand.ts', context);
        }
        return nextResolve(specifier, context);
    }
});

function reset(trusted, fail = false) {
    Object.assign(state, { trusted, fail, starts: 0, configReads: 0, error: undefined, command: undefined });
    return { subscriptions: [] };
}

test('untrusted workspace waits before reading executable configuration or starting', async () => {
    const context = reset(false);
    const extension = await import('../src/extension.ts?trust');
    await extension.activate(context);
    assert.equal(state.starts, 0);
    assert.equal(state.configReads, 0);
    state.trusted = true;
    state.grant();
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(state.starts, 1);
    assert.equal(state.command, 'aden-lsp');
    await extension.deactivate();
});

test('startup failure is caught and offers an actionable command and setting', async () => {
    const context = reset(true, true);
    const extension = await import('../src/extension.ts?failure');
    await extension.activate(context);
    assert.equal(state.starts, 1);
    assert.match(state.error, /aden-lsp/);
    assert.match(state.error, /aden.lspPath/);
    assert.match(state.output, /ENOENT/);
    state.fail = false;
    await extension.activate(context);
    assert.equal(state.starts, 2);
    await extension.deactivate();
});
