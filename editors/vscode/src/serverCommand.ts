import * as path from 'path';

/** Keep bare commands on PATH; resolve explicitly configured relative paths. */
export function resolveLspCommand(configured: string | undefined, root: string | undefined): string {
    const command = configured?.trim() || 'aden-lsp';
    if (path.isAbsolute(command) || !/[\\/]/.test(command)) {
        return command;
    }
    if (!root) {
        throw new Error('A relative aden.lspPath requires an open workspace folder. Use an absolute path or a command on PATH.');
    }
    return path.resolve(root, command);
}
