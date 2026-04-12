const fs = require("node:fs");
const path = require("node:path");
const vscode = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");

let client;

async function activate(context) {
  const command = resolveServerCommand();
  const clientOptions = {
    documentSelector: [{ language: "thebe", scheme: "file" }],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher("**/*.{trs,toml,html}"),
    },
  };

  client = new LanguageClient(
    "thebe-lsp",
    "Thebe Language Server",
    {
      run: { command },
      debug: { command },
    },
    clientOptions,
  );

  context.subscriptions.push(client.start());
}

function resolveServerCommand() {
  const configured = vscode.workspace.getConfiguration("thebe").get("lsp.path");
  if (configured && typeof configured === "string" && configured.length > 0) {
    return configured;
  }

  for (const folder of vscode.workspace.workspaceFolders ?? []) {
    const candidate = path.join(folder.uri.fsPath, "target", "debug", "thebe-lsp");
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }

  return "thebe-lsp";
}

async function deactivate() {
  if (client) {
    await client.stop();
  }
}

module.exports = {
  activate,
  deactivate,
};
