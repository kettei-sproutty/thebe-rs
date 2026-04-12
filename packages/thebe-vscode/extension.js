const fs = require("node:fs");
const path = require("node:path");
const vscode = require("vscode");
const { LanguageClient } = require("vscode-languageclient/node");

let client;

async function activate(context) {
  const command = resolveServerCommand(context);
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

function resolveServerCommand(context) {
  const configured = vscode.workspace.getConfiguration("thebe").get("lsp.path");
  if (configured && typeof configured === "string" && configured.length > 0) {
    return configured;
  }

  const bundled = bundledServerCommand(context.extensionPath);
  if (bundled) {
    return bundled;
  }

  for (const folder of vscode.workspace.workspaceFolders ?? []) {
    const candidate = path.join(folder.uri.fsPath, "target", "debug", "thebe-lsp");
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }

  return "thebe-lsp";
}

function bundledServerCommand(extensionPath) {
  const executable = process.platform === "win32" ? "thebe-lsp.exe" : "thebe-lsp";
  const candidate = path.join(extensionPath, "bin", executable);
  if (fs.existsSync(candidate)) {
    return candidate;
  }

  return null;
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
