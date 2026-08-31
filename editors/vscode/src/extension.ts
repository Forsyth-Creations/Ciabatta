/**
 * The VS Code half of ciabatta's editor support.
 *
 * There are two pieces, and they know different things.
 *
 * The **JSON Schemas** in `schemas/` describe the shape of the files — which
 * fields exist, what each takes, what it's for. They're registered through the
 * `yamlValidation` contribution point in `package.json`, which VS Code's YAML
 * extension picks up; nothing in this file is involved, and they keep working
 * with no `ciabatta` binary installed.
 *
 * The **language server** is `ciabatta lsp`, and it knows the things a schema
 * cannot: which sub-workspaces this monorepo contains, which workflows they
 * define, which tools the root promises to install. All this file does is find
 * that binary and point it at the right files.
 */

import { execFile } from "node:child_process";
import { promisify } from "node:util";
import * as vscode from "vscode";
import {
  LanguageClient,
  type LanguageClientOptions,
  type ServerOptions,
} from "vscode-languageclient/node";

const run = promisify(execFile);

/** The one server instance, so a restart can stop the old one first. */
let client: LanguageClient | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(
    vscode.commands.registerCommand("ciabatta.restartServer", () => start(context, true)),
  );

  // A changed binary path is a different server; a toggled `enabled` is a
  // server that should or shouldn't be running at all.
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (event.affectsConfiguration("ciabatta.server")) start(context, true);
    }),
  );

  await start(context, false);
}

export async function deactivate(): Promise<void> {
  await client?.stop();
  client = undefined;
}

/**
 * Start the server, replacing any already running one.
 *
 * `announce` distinguishes the two reasons we get here: on activation a
 * missing binary is unremarkable — plenty of people will have the extension
 * before they have the CLI, and the schemas work regardless — but when someone
 * has just pointed a setting at a binary, silence would be a bug.
 */
async function start(context: vscode.ExtensionContext, announce: boolean): Promise<void> {
  await client?.stop();
  client = undefined;

  const settings = vscode.workspace.getConfiguration("ciabatta");
  if (!settings.get<boolean>("server.enabled", true)) return;

  const command = settings.get<string>("server.path")?.trim() || "ciabatta";
  const version = await serverVersion(command);
  if (!version) {
    if (announce) {
      void vscode.window.showWarningMessage(
        `Couldn't run \`${command} lsp\`. Field completion from the JSON Schema still works; ` +
          "cross-workspace completion needs the ciabatta CLI on your PATH, or " +
          "`ciabatta.server.path` pointed at it.",
      );
    }
    return;
  }

  // No `transport`, even though stdio is what this is. Naming it appends
  // `--stdio` to the arguments, which older ciabatta binaries reject: they
  // exit 2 before reading a byte, and the client restarts them until it gives
  // up, which from the editor's side is a server that keeps crashing. Current
  // ones accept and ignore the flag, so leaving it off is what makes the
  // extension work against binaries people already have installed.
  const server: ServerOptions = {
    command,
    args: ["lsp"],
  };

  const options: LanguageClientOptions = {
    // Only ciabatta's own files. Every other YAML document in the workspace
    // belongs to some other tool, and this server has nothing to say about it.
    documentSelector: [
      { scheme: "file", language: "yaml", pattern: "**/.ciabatta/ciabatta.{yaml,yml}" },
      { scheme: "file", language: "yaml", pattern: "**/.ciabatta/workflows/*.{yaml,yml}" },
    ],
    synchronize: {
      // A member added or removed elsewhere in the repo changes what every
      // open file may refer to.
      fileEvents: vscode.workspace.createFileSystemWatcher("**/.ciabatta/**/*.{yaml,yml,toml}"),
    },
    outputChannelName: "Ciabatta",
  };

  const started = new LanguageClient("ciabatta", "Ciabatta", server, options);
  await started.start();
  client = started;
  context.subscriptions.push(started);

  if (announce) {
    void vscode.window.showInformationMessage(`Ciabatta language server running (${version}).`);
  }
}

/**
 * The binary's version, or `undefined` if it isn't there or isn't ciabatta.
 *
 * Asking before launching turns "the extension silently does nothing" into a
 * message that names the command it tried.
 */
async function serverVersion(command: string): Promise<string | undefined> {
  try {
    const { stdout } = await run(command, ["--version"], { timeout: 5_000 });
    return stdout.trim() || undefined;
  } catch {
    return undefined;
  }
}
