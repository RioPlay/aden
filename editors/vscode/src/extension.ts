import { ExtensionContext, workspace, window } from 'vscode';
import {
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    TransportKind
} from 'vscode-languageclient/node';
import { resolveLspCommand } from './serverCommand';

let client: LanguageClient | undefined;

export async function activate(context: ExtensionContext): Promise<void> {
    // Workspace configuration can point at executable code. Wait for trust before
    // reading it or launching a server, including a relative workspace binary.
    if (!workspace.isTrusted) {
        context.subscriptions.push(workspace.onDidGrantWorkspaceTrust(() => {
            void startClient(context);
        }));
        return;
    }
    await startClient(context);
}

async function startClient(context: ExtensionContext): Promise<void> {
    if (client || !workspace.isTrusted) {
        return;
    }
    const output = window.createOutputChannel('Aden Language Server');
    context.subscriptions.push(output);
    let command = 'aden-lsp';
    try {
        const root = workspace.workspaceFolders?.[0]?.uri.fsPath;
        command = resolveLspCommand(workspace.getConfiguration('aden').get<string>('lspPath'), root);
        const serverOptions: ServerOptions = {
            command,
            args: [],
            options: { cwd: root },
            transport: TransportKind.stdio
        };
        const clientOptions: LanguageClientOptions = {
            documentSelector: [{ scheme: 'file', language: 'asciidoc' }],
            outputChannel: output,
            synchronize: {
                fileEvents: workspace.createFileSystemWatcher('**/*.{adoc,aden}')
            }
        };
        const startingClient = new LanguageClient('adenLsp', 'Aden Language Server', serverOptions, clientOptions);
        client = startingClient;
        await startingClient.start();
    } catch (error) {
        client = undefined;
        const detail = error instanceof Error ? error.message : String(error);
        output.appendLine(`Could not start ${command}: ${detail}`);
        const action = await window.showErrorMessage(
            `Aden could not start "${command}". Install aden-lsp on PATH or set aden.lspPath to an executable. ${detail}`,
            'Open Settings', 'Show Output'
        );
        if (action === 'Open Settings') {
            const { commands } = await import('vscode');
            await commands.executeCommand('workbench.action.openSettings', 'aden.lspPath');
        } else if (action === 'Show Output') {
            output.show();
        }
    }
}

export function deactivate(): Thenable<void> | undefined {
    return client?.stop();
}
