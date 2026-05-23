import * as path from 'path';
import { ExtensionContext, workspace } from 'vscode';
import {
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    TransportKind
} from 'vscode-languageclient/node';

let client: LanguageClient;

export function activate(context: ExtensionContext) {
    const config = workspace.getConfiguration('aden');
    let lspPath = config.get<string>('lspPath', 'aden-lsp');

    // Try workspace binary first
    const workspaceBinary = path.join(context.extensionPath, '..', '..', 'target', 'debug', 'aden-lsp');
    if (lspPath === 'aden-lsp') {
        lspPath = workspaceBinary;
    }

    const serverOptions: ServerOptions = {
        command: lspPath,
        args: [],
        options: { cwd: workspace.rootPath || undefined },
        transport: TransportKind.stdio
    };

    const clientOptions: LanguageClientOptions = {
        documentSelector: [{ scheme: 'file', language: 'asciidoc' }],
        synchronize: {
            fileEvents: workspace.createFileSystemWatcher('**/.{adoc,aden}')
        }
    };

    client = new LanguageClient('adenLsp', 'Aden Language Server', serverOptions, clientOptions);
    client.start();
}

export function deactivate(): Thenable<void> | undefined {
    if (!client) {
        return undefined;
    }
    return client.stop();
}
