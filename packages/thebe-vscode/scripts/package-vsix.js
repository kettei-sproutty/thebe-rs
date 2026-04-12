const childProcess = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const packageRoot = path.resolve(__dirname, "..");
const repoRoot = path.resolve(packageRoot, "..", "..");
const executableName = process.platform === "win32" ? "thebe-lsp.exe" : "thebe-lsp";

function main() {
  const options = parseArgs(process.argv.slice(2));
  const serverPath = prepareServerBinary(options);
  const bundledPath = bundleServerBinary(serverPath);
  const outputPath = packageOutputPath(options.target);

  console.log(`Bundled language server: ${bundledPath}`);
  run(
    npxCommand(),
    [
      "@vscode/vsce",
      "package",
      "--skip-license",
      "--target",
      options.target,
      "--out",
      outputPath,
    ],
    packageRoot,
  );

  console.log(`Created VSIX: ${outputPath}`);
}

function parseArgs(argv) {
  const options = {
    profile: "release",
    server: null,
    target: `${process.platform}-${process.arch}`,
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--profile") {
      const value = argv[index + 1];
      if (value !== "debug" && value !== "release") {
        throw new Error("--profile must be either debug or release");
      }
      options.profile = value;
      index += 1;
      continue;
    }

    if (arg === "--server") {
      const value = argv[index + 1];
      if (!value) {
        throw new Error("--server requires an absolute path to thebe-lsp");
      }
      options.server = path.resolve(value);
      index += 1;
      continue;
    }

    if (arg === "--target") {
      const value = argv[index + 1];
      if (!value) {
        throw new Error("--target requires a VS Code target such as darwin-arm64");
      }
      options.target = value;
      index += 1;
      continue;
    }

    throw new Error(`Unknown argument: ${arg}`);
  }

  return options;
}

function prepareServerBinary(options) {
  if (options.server) {
    ensureFileExists(options.server, "Configured thebe-lsp binary was not found");
    return options.server;
  }

  const cargoArgs = ["build", "-p", "thebe-lsp"];
  if (options.profile === "release") {
    cargoArgs.push("--release");
  }

  run(cargoCommand(), cargoArgs, repoRoot);

  const builtBinary = path.join(repoRoot, "target", options.profile, executableName);
  ensureFileExists(builtBinary, "Built thebe-lsp binary was not found after cargo build");
  return builtBinary;
}

function bundleServerBinary(serverPath) {
  const binDir = path.join(packageRoot, "bin");
  const bundledPath = path.join(binDir, executableName);

  fs.mkdirSync(binDir, { recursive: true });
  fs.copyFileSync(serverPath, bundledPath);
  if (process.platform !== "win32") {
    fs.chmodSync(bundledPath, 0o755);
  }

  return bundledPath;
}

function packageOutputPath(target) {
  const manifestPath = path.join(packageRoot, "package.json");
  const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  return path.join(packageRoot, `${manifest.name}-${manifest.version}-${target}.vsix`);
}

function ensureFileExists(filePath, message) {
  if (!fs.existsSync(filePath)) {
    throw new Error(`${message}: ${filePath}`);
  }
}

function run(command, args, cwd) {
  childProcess.execFileSync(command, args, {
    cwd,
    stdio: "inherit",
  });
}

function cargoCommand() {
  return process.platform === "win32" ? "cargo.exe" : "cargo";
}

function npxCommand() {
  return process.platform === "win32" ? "npx.cmd" : "npx";
}

main();
