const fs = require("node:fs");
const path = require("node:path");

const FILE_WATCH_GLOB = "**/*.{trs,toml,html}";

function createDocumentSelector() {
  return [{ language: "thebe" }];
}

function resolveServerCommand({ configuredPath, extensionPath, workspaceFolders, platform = process.platform }) {
  if (typeof configuredPath === "string" && configuredPath.length > 0) {
    return configuredPath;
  }

  const developmentCheckout = developmentCheckoutServerCommand(extensionPath, platform);
  if (developmentCheckout) {
    return developmentCheckout;
  }

  const bundled = bundledServerCommand(extensionPath, platform);
  if (bundled) {
    return bundled;
  }

  const workspace = workspaceDebugServerCommand(workspaceFolders, platform);
  if (workspace) {
    return workspace;
  }

  return executableName(platform);
}

function bundledServerCommand(extensionPath, platform = process.platform) {
  if (!extensionPath) {
    return null;
  }

  const candidate = path.join(extensionPath, "bin", executableName(platform));
  if (fs.existsSync(candidate)) {
    return candidate;
  }

  return null;
}

function developmentCheckoutServerCommand(extensionPath, platform = process.platform) {
  if (!extensionPath) {
    return null;
  }

  let current = path.resolve(extensionPath);
  for (let depth = 0; depth < 4; depth += 1) {
    const candidate = path.join(current, "target", "debug", executableName(platform));
    if (fs.existsSync(candidate)) {
      return candidate;
    }

    const parent = path.dirname(current);
    if (parent === current) {
      break;
    }
    current = parent;
  }

  return null;
}

function workspaceDebugServerCommand(workspaceFolders, platform = process.platform) {
  for (const folder of workspaceFolders ?? []) {
    const candidate = path.join(folder, "target", "debug", executableName(platform));
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }

  return null;
}

function executableName(platform = process.platform) {
  return platform === "win32" ? "thebe-lsp.exe" : "thebe-lsp";
}

module.exports = {
  createDocumentSelector,
  FILE_WATCH_GLOB,
  resolveServerCommand,
};
